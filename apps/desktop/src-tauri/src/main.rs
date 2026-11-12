// Prevents an extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Elevated helper mode: `veranda-desktop --hosts-apply <payload>` writes the hosts file
    // and exits without touching the GUI.
    let args: Vec<String> = std::env::args().collect();
    if args.len() == 3 && args[1] == "--hosts-apply" {
        let hosts_file = veranda_core::Platform::current().hosts_file();
        let payload = std::path::PathBuf::from(&args[2]);
        let code = match veranda_core::hosts::apply_payload(&hosts_file, &payload) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("{e:#}");
                1
            }
        };
        let _ = std::fs::remove_file(&payload);
        std::process::exit(code);
    }
    // WebKitGTK's DMA-BUF (GPU) renderer is on by default; `general.hardware_acceleration = false`
    // opts out for drivers that flicker. An explicit env var always wins.
    // Rasterise page content on the CPU but keep GPU compositing: scrolling stays at 60 fps
    // (measured) while the GPU-accelerated Skia raster path, which crashes WebKitGTK's web process
    // on some Intel/Mesa drivers, is not used. An explicit env var always wins.
    #[cfg(target_os = "linux")]
    if std::env::var_os("WEBKIT_SKIA_ENABLE_CPU_RENDERING").is_none() {
        // SAFETY: called before any other thread exists (start of main).
        unsafe { std::env::set_var("WEBKIT_SKIA_ENABLE_CPU_RENDERING", "1") };
    }
    #[cfg(target_os = "linux")]
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        let gpu = veranda_core::Veranda::open()
            .map(|p| p.config.general.hardware_acceleration)
            .unwrap_or(true);
        if !gpu {
            // SAFETY: called before any other thread exists (start of main).
            unsafe { std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1") };
        }
    }
    harden_process();
    veranda_desktop_lib::run()
}

/// Keep secrets out of crash dumps: the app (and every helper it spawns, which inherit the
/// limit) never writes a core file, and other processes of the same user cannot ptrace it. Crash
/// reporters such as Apport upload core dumps that would contain whatever the UI held in memory,
/// for example a password being typed.
#[cfg(unix)]
fn harden_process() {
    if std::env::var_os("VERANDA_ALLOW_COREDUMP").is_some() {
        return;
    }
    // SAFETY: plain libc calls with valid arguments, made before any other thread exists.
    unsafe {
        let none = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        let _ = libc::setrlimit(libc::RLIMIT_CORE, &none);
        #[cfg(target_os = "linux")]
        let _ = libc::prctl(libc::PR_SET_DUMPABLE, 0);
    }
}

#[cfg(not(unix))]
fn harden_process() {}
