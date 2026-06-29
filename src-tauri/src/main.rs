// Sem isto, o binário release no Windows roda como console app e o Windows
// abre/mantém uma janela de terminal junto da janela do Tauri. O atributo
// remove o console no release (mantém no debug para ver logs).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    remnant_finder_drive_lib::run()
}
