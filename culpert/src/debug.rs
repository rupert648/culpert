// Debug logging gated by CULPERT_DEBUG=1 in env. Read once at startup; the
// hot-path check is one Relaxed atomic load.
use std::sync::atomic::{AtomicBool, Ordering};
static ENABLED: AtomicBool = AtomicBool::new(false);
static INIT: std::sync::Once = std::sync::Once::new();

pub(crate) fn enabled() -> bool {
    INIT.call_once(|| {
        if std::env::var_os("CULPERT_DEBUG").is_some() {
            ENABLED.store(true, Ordering::Relaxed);
        }
    });
    ENABLED.load(Ordering::Relaxed)
}

macro_rules! dbglog { ($($arg:tt)*) => {
        if $crate::debug::enabled() {
            use std::io::Write as _;
            let _ = writeln!(std::io::stderr(), "[culpert tid={:?}] {}",
                std::thread::current().id(), format_args!($($arg)*));
        }
    };
}

pub(crate) use dbglog;
