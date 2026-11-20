use rodio::{Decoder, OutputStream, Sink, Source};

use std::collections::VecDeque;
use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// Команды аудиопотоку.
#[derive(Debug)]
pub enum AudioCommand {
    Play {
        path: PathBuf,
        /// Длительность из тегов — используется, если декодер не смог
        /// определить её сам (для многих MP3 `total_duration()` равен None).
        fallback_duration: Duration,
    },
    Pause,
    Resume,
    Stop,
    Seek(Duration),
    SetVolume(f32),
}

/// Состояние воспроизведения.
#[derive(Clone, Debug)]
pub struct AudioState {
    pub is_playing: bool,
    pub is_paused: bool,
    pub position: Duration,
    pub duration: Duration,
    pub volume: f32,
    pub error: Option<String>,
    /// Текущий sink полностью опустел — трек действительно доиграл.
    /// В отличие от `position >= duration`, работает и при нулевой/неверной
    /// длительности.
    pub finished: bool,
    /// Ошибка относится к воспроизведению (Play/Sink), а не к перемотке.
    /// Только такие ошибки должны автоматически переключать трек.
    pub error_is_fatal: bool,
}

impl Default for AudioState {
    fn default() -> Self {
        Self {
            is_playing: false,
            is_paused: false,
            position: Duration::ZERO,
            duration: Duration::ZERO,
            volume: 0.7,
            error: None,
            finished: false,
            error_is_fatal: false,
        }
    }
}

/// Управление аудио через канал.
/// OutputStream и Sink остаются исключительно в аудиопотоке.
pub struct AudioController {
    pub command_tx: Sender<AudioCommand>,
    pub state: Arc<Mutex<AudioState>>,
}

impl AudioController {
    pub fn new() -> Self {
        let (command_tx, command_rx) = mpsc::channel();
        let state = Arc::new(Mutex::new(AudioState::default()));
        let thread_state = Arc::clone(&state);

        thread::spawn(move || {
            audio_thread(command_rx, thread_state);
        });

        Self { command_tx, state }
    }

    pub fn send(&self, command: AudioCommand) {
        if self.command_tx.send(command).is_err() {
            if let Ok(mut state) = self.state.lock() {
                state.error = Some("Аудиопоток завершился".to_string());
                state.error_is_fatal = true;
                state.is_playing = false;
                state.is_paused = false;
                state.finished = false;
            }
        }
    }

    pub fn get_state(&self) -> AudioState {
        self.state.lock().map(|state| state.clone()).unwrap_or_default()
    }
}

/// Аудиопоток. OutputStream не передаётся между потоками.
fn audio_thread(command_rx: Receiver<AudioCommand>, shared_state: Arc<Mutex<AudioState>>) {
    let (_stream, stream_handle) = match OutputStream::try_default() {
        Ok(output) => output,
        Err(err) => {
            if let Ok(mut state) = shared_state.lock() {
                state.error = Some(format!("Не удалось открыть аудиоустройство: {err}"));
                state.error_is_fatal = true;
                state.is_playing = false;
                state.is_paused = false;
                state.finished = false;
            }
            return;
        }
    };

    let mut sink: Option<Sink> = None;
    let mut current_duration = Duration::ZERO;
    let mut volume = 0.7_f32;
    // Команды, отложенные коалесингом `Play` (см. `queue_command`).
    let mut pending: VecDeque<AudioCommand> = VecDeque::new();

    loop {
        // Сначала выполняем отложенные команды, потом читаем канал.
        let command = match pending.pop_front() {
            Some(command) => Some(command),
            None => match command_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(command) => {
                    queue_command(command, &command_rx, &mut pending);
                    pending.pop_front()
                }
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            },
        };

        if let Some(command) = command {
            match command {
                AudioCommand::Play {
                    path,
                    fallback_duration,
                } => {
                    // Сбрасываем состояние прошлого трека сразу при получении
                    // команды, ещё до открытия файла: иначе UI успеет увидеть
                    // старое «играет»/«доиграл» и перескочить через трек,
                    // пока декодер только открывается.
                    if let Ok(mut state) = shared_state.lock() {
                        state.error = None;
                        state.error_is_fatal = false;
                        state.finished = false;
                        state.is_playing = false;
                        state.is_paused = false;
                        state.position = Duration::ZERO;
                        state.duration = fallback_duration;
                    }

                    if let Some(old_sink) = sink.take() {
                        old_sink.stop();
                    }

                    // Большие файлы не открываем: Decoder::new/total_duration()
                    // умеют сканировать весь файл и блокировать аудиопоток
                    // навсегда — после этого не переключается ни один трек.
                    let file_size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                    const MAX_PLAYABLE_SIZE: u64 = 100 * 1024 * 1024; // 100 MB
                    if file_size > MAX_PLAYABLE_SIZE {
                        // Фатальная ошибка: check_track_finished сам переключит
                        // на следующий трек (как при битом файле), поэтому
                        // слишком большие файлы автоматически пропускаются.
                        set_error(
                            &shared_state,
                            format!(
                                "Файл слишком большой ({} MB). Максимум 100 MB.",
                                file_size / 1024 / 1024
                            ),
                        );
                        continue; // пропускаем этот Play, не открываем Decoder
                    }

                    // Любая ошибка на этом пути (файл не найден, битый формат,
                    // неподдерживаемый кодек) попадает в state.error, а не в panic.
                    match File::open(&path) {
                        Ok(file) => {
                            let reader = BufReader::new(file);
                            match Decoder::new(reader) {
                                Ok(source) => {
                                    // Приоритет у метаданных: rodio для VBR MP3
                                    // иногда отдаёт ненулевую, но неверную оценку.
                                    let decoder_duration =
                                        source.total_duration().filter(|d| !d.is_zero());
                                    let duration = if fallback_duration.is_zero() {
                                        decoder_duration.unwrap_or(Duration::ZERO)
                                    } else {
                                        fallback_duration
                                    };

                                    if let Some(decoder_duration) = decoder_duration {
                                        if !fallback_duration.is_zero()
                                            && decoder_duration.abs_diff(fallback_duration)
                                                > Duration::from_secs(1)
                                        {
                                            eprintln!(
                                                "Длительность '{}' расходится: метаданные {:?}, декодер {:?}",
                                                path.display(),
                                                fallback_duration,
                                                decoder_duration
                                            );
                                        }
                                    }

                                    match Sink::try_new(&stream_handle) {
                                        Ok(new_sink) => {
                                            new_sink.set_volume(volume);
                                            new_sink.append(source);

                                            current_duration = duration;
                                            sink = Some(new_sink);

                                            if let Ok(mut state) = shared_state.lock() {
                                                state.is_playing = true;
                                                state.is_paused = false;
                                                state.position = Duration::ZERO;
                                                state.duration = duration;
                                                state.volume = volume;
                                                state.error = None;
                                                state.error_is_fatal = false;
                                                state.finished = false;
                                            }
                                        }
                                        Err(err) => {
                                            set_error(&shared_state, format!("Ошибка Sink: {err}"));
                                        }
                                    }
                                }
                                Err(err) => {
                                    let ext = path
                                        .extension()
                                        .map(|e| e.to_string_lossy().to_string())
                                        .unwrap_or_else(|| "?".to_string());
                                    set_error(
                                        &shared_state,
                                        format!(
                                            "Ошибка декодирования '{}' (.{ext}): {err}",
                                            path.display()
                                        ),
                                    );
                                }
                            }
                        }
                        Err(err) => {
                            set_error(
                                &shared_state,
                                format!("Ошибка открытия файла '{}': {err}", path.display()),
                            );
                        }
                    }
                }

                AudioCommand::Pause => {

                    if let Some(ref current_sink) = sink {
                        current_sink.pause();
                        if let Ok(mut state) = shared_state.lock() {
                            state.is_paused = true;
                            state.is_playing = false;
                        }
                    }
                }

                AudioCommand::Resume => {
                    if let Some(ref current_sink) = sink {
                        current_sink.play();
                        if let Ok(mut state) = shared_state.lock() {
                            state.is_paused = false;
                            state.is_playing = true;
                        }
                    }
                }

                AudioCommand::Stop => {
                    // Полная остановка: в отличие от Pause, здесь sink
                    // пересоздаётся с нуля (take() + stop()), поэтому после
                    // Stop плеер гарантированно не удерживает ссылку на
                    // старый (возможно, уже удалённый из плейлиста) файл.
                    if let Some(old_sink) = sink.take() {
                        old_sink.stop();
                    }
                    current_duration = Duration::ZERO;
                    if let Ok(mut state) = shared_state.lock() {
                        state.is_playing = false;
                        state.is_paused = false;
                        state.position = Duration::ZERO;
                        state.duration = Duration::ZERO;
                        state.finished = false;
                        state.error = None;
                        state.error_is_fatal = false;
                    }
                }

                AudioCommand::SetVolume(value) => {
                    volume = value.clamp(0.0, 1.0);
                    if let Some(ref current_sink) = sink {
                        current_sink.set_volume(volume);
                    }
                    if let Ok(mut state) = shared_state.lock() {
                        state.volume = volume;
                    }
                }

                AudioCommand::Seek(position) => {
                    if let Some(ref current_sink) = sink {
                        // shortcut: rodio ждёт ответа аудиоколбэка cpal без
                        // таймаута — если устройство вывода пропало, поток
                        // команд встанет здесь навсегда. Починка требует
                        // правки rodio (Sink не Clone, таймаута нет).
                        match current_sink.try_seek(position) {
                            Ok(()) => {
                                if let Ok(mut state) = shared_state.lock() {
                                    // Успешная перемотка отменяет «доиграл»:
                                    // иначе UI может переключить трек, пока
                                    // аудиопоток ещё не обновил позицию.
                                    state.finished = false;
                                    state.position = position;
                                }
                            }
                            Err(err) => {
                                // Ошибка перемотки нефатальна (трек играет
                                // дальше), поэтому в общее состояние её не
                                // пишем: UI показывал бы её вместо
                                // исполнителя до следующего Play.
                                eprintln!("Ошибка перемотки: {err}");
                            }
                        }
                    }
                }
            }
        }

        if let Some(ref current_sink) = sink {
            let position = current_sink.get_pos();
            let empty = current_sink.empty();

            if let Ok(mut state) = shared_state.lock() {
                state.position = position;
                state.duration = current_duration;
                state.is_playing = !current_sink.is_paused() && !empty;
                state.finished = empty;

                if empty {
                    state.is_playing = false;
                    state.is_paused = false;
                    // Если трек доиграл до конца, ставим позицию ровно в длительность,
                    // чтобы UI точно понял, что пора переключать трек.
                    if current_duration > Duration::ZERO && position > Duration::ZERO {
                        state.position = current_duration;
                    }
                }
            }
        }
    }
}

/// Кладёт команду в отложенную очередь. Для `Play` дополнительно вычитывает
/// уже накопившиеся команды и оставляет только последний `Play`: UI мог
/// накликать несколько треков, пока поток был занят открытием файла, и
/// промежуточные треки иначе начинали бы играть. Остальные команды
/// сохраняются в исходном порядке и выполнятся раньше последнего `Play`.
fn queue_command(
    command: AudioCommand,
    command_rx: &Receiver<AudioCommand>,
    pending: &mut VecDeque<AudioCommand>,
) {
    if !matches!(command, AudioCommand::Play { .. }) {
        pending.push_back(command);
        return;
    }

    // Пачка: текущий Play и всё, что успело накопиться в канале.
    let mut batch: VecDeque<AudioCommand> = VecDeque::new();
    batch.push_back(command);
    while let Ok(next) = command_rx.try_recv() {
        batch.push_back(next);
    }

    // Идём с конца: первый встреченный Play — последний, его и оставляем.
    let mut last_play_seen = false;
    for queued in batch.into_iter().rev() {
        if matches!(queued, AudioCommand::Play { .. }) {
            if last_play_seen {
                continue; // устаревший Play: этот трек запускать не нужно
            }
            last_play_seen = true;
        }
        pending.push_front(queued);
    }
}

fn set_error(shared_state: &Arc<Mutex<AudioState>>, message: String) {
    if let Ok(mut state) = shared_state.lock() {
        state.error = Some(message);
        state.error_is_fatal = true;
        // Ошибка воспроизведения: звука нет, UI не должен показывать «играет».
        state.is_playing = false;
        state.is_paused = false;
        // Ошибка обрабатывается отдельной веткой UI; не даём старому
        // «finished» инициировать ещё одно переключение.
        state.finished = false;
    }
}

#[cfg(test)]
mod tests {
    use super::{AudioCommand, queue_command};
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::time::Duration;

    fn play(path: &'static str) -> AudioCommand {
        AudioCommand::Play {
            path: PathBuf::from(path),
            fallback_duration: Duration::from_secs(1),
        }
    }

    /// Кладёт `command` в очередь, предварительно отправив `sent` в канал.
    fn pending_after(command: AudioCommand, sent: Vec<AudioCommand>) -> Vec<AudioCommand> {
        let (sender, receiver) = mpsc::channel();
        for next in sent {
            let _ = sender.send(next);
        }
        let mut pending = VecDeque::new();
        queue_command(command, &receiver, &mut pending);
        pending.into_iter().collect()
    }

    #[test]
    fn keeps_only_last_play() {
        let pending = pending_after(play("a"), vec![play("b"), play("c")]);
        assert_eq!(pending.len(), 1);
        match &pending[0] {
            AudioCommand::Play { path, .. } => assert_eq!(path, &PathBuf::from("c")),
            other => panic!("ожидался последний Play, получено {other:?}"),
        }
    }

    #[test]
    fn keeps_other_commands_in_order() {
        let pending = pending_after(
            play("a"),
            vec![AudioCommand::SetVolume(0.3), play("b"), AudioCommand::Stop],
        );
        assert_eq!(pending.len(), 3);
        assert!(matches!(&pending[0], AudioCommand::SetVolume(volume) if *volume == 0.3));
        match &pending[1] {
            AudioCommand::Play { path, .. } => assert_eq!(path, &PathBuf::from("b")),
            other => panic!("ожидался Play, получено {other:?}"),
        }
        assert!(matches!(&pending[2], AudioCommand::Stop));
    }
}
