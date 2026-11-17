use rodio::{Decoder, OutputStream, Sink, Source};

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
            }
            return;
        }
    };

    let mut sink: Option<Sink> = None;
    let mut current_duration = Duration::ZERO;
    let mut volume = 0.7_f32;

    loop {
        match command_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(command) => match command {
                AudioCommand::Play {
                    path,
                    fallback_duration,
                } => {
                    // Сбрасываем «доиграл» сразу при получении команды, ещё до
                    // открытия файла: иначе UI успеет увидеть старое значение
                    // и перескочить через трек, пока декодер только открывается.
                    if let Ok(mut state) = shared_state.lock() {
                        state.finished = false;
                    }

                    if let Some(old_sink) = sink.take() {
                        old_sink.stop();
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
                                    let decoder_duration = source
                                        .total_duration()
                                        .filter(|duration| !duration.is_zero());
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
                        if let Err(err) = current_sink.try_seek(position) {
                            set_error(&shared_state, format!("Ошибка перемотки: {err}"));
                        }
                    }
                }
            },

            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
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

fn set_error(shared_state: &Arc<Mutex<AudioState>>, message: String) {
    if let Ok(mut state) = shared_state.lock() {
        state.error = Some(message);
        // Ошибка обрабатывается отдельной веткой UI; не даём старому
        // «finished» инициировать ещё одно переключение.
        state.finished = false;
    }
}
