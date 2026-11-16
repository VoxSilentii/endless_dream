//! Встраивает иконку и описание в `.exe` для проводника Windows.
//!
//! Сам egui рисует иконку окна и трея из PNG в рантайме; этот скрипт нужен
//! только для того, чтобы у файла `endless_dream.exe` была иконка в
//! проводнике и корректное имя в свойствах.

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");

    #[cfg(windows)]
    {
        let mut res = winres::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set("ProductName", "Endless Dream");
        res.set("FileDescription", "Endless Dream — музыкальный плеер");

        // Отсутствие rc.exe/Windows SDK не должно валить сборку: играет
        // роль только оформление exe-файла, само приложение от него не
        // зависит.
        if let Err(error) = res.compile() {
            println!("cargo:warning=не удалось встроить иконку .exe: {error}");
        }
    }
}
