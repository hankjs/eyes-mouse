mod calibration;
mod gestures;
mod macos;
mod refinement;
mod runtime;
mod snake;

fn main() {
    if std::env::args_os()
        .nth(1)
        .is_some_and(|argument| argument == macos::CHECK_ACCESSIBILITY_PERMISSION)
    {
        let granted = macos::accessibility_trusted() || macos::can_move_pointer();
        std::process::exit(if granted { 0 } else { 1 });
    }
    runtime::run();
}
