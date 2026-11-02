#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    let interactive = std::env::args()
        .nth(1)
        .is_none_or(|arg| arg == "--data-dir");
    if let Err(error) = snapper_desktop::platform::initialize_process()
        .and_then(|_| snapper_desktop::media::entry())
    {
        eprintln!("{error:#}");
        if interactive {
            let message =
                windows::core::HSTRING::from(format!("Snapper could not start.\n\n{error:#}"));
            // SAFETY: Both strings are live and this dialog belongs to this application's startup.
            unsafe {
                windows::Win32::UI::WindowsAndMessaging::MessageBoxW(
                    None,
                    &message,
                    windows::core::w!("Snapper"),
                    windows::Win32::UI::WindowsAndMessaging::MB_OK
                        | windows::Win32::UI::WindowsAndMessaging::MB_ICONERROR,
                );
            }
        }
        std::process::exit(1);
    }
}
