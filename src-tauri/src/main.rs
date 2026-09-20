#![cfg_attr(
    any(not(debug_assertions), feature = "gui-subsystem"),
    windows_subsystem = "windows"
)]

fn main() {
    alldownloader_lib::run();
}
