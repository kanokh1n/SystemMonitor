// Не открывать лишнее окно консоли в релизной сборке под Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    system_monitor_lib::run()
}
