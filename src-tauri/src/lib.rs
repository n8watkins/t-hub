mod commands;
mod pty;
mod tmux;

use commands::TerminalManager;

pub fn run() {
    tauri::Builder::default()
        .manage(TerminalManager::default())
        .invoke_handler(tauri::generate_handler![
            commands::spawn_terminal,
            commands::attach_terminal,
            commands::write_terminal,
            commands::resize_terminal,
            commands::close_terminal,
            commands::kill_terminal,
            commands::list_terminals,
        ])
        .run(tauri::generate_context!())
        .expect("error while running TermHub");
}
