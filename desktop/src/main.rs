#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    let interactive = std::env::args()
        .nth(1)
        .is_none_or(|arg| arg == "--data-dir");
    if let Err(error) =
        steadycapture::platform::initialize_process().and_then(|_| steadycapture::media::entry())
    {
        eprintln!("{error:#}");
        if interactive {
            let message = windows::core::HSTRING::from(format!(
                "SteadyCapture could not start.\n\n{error:#}"
            ));
            // SAFETY: Both strings are live and this dialog belongs to this application's startup.
            unsafe {
                windows::Win32::UI::WindowsAndMessaging::MessageBoxW(
                    None,
                    &message,
                    windows::core::w!("SteadyCapture"),
                    windows::Win32::UI::WindowsAndMessaging::MB_OK
                        | windows::Win32::UI::WindowsAndMessaging::MB_ICONERROR,
                );
            }
        }
        std::process::exit(1);
    }
}
