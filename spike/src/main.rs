//! Phase 0 research spike — throwaway code answering the five load-bearing
//! questions in `plan.md` § "Phase 0 — research spike". Verdicts are written
//! up in `notes.md` at the workspace root.
//!
//!   cargo run -p spike -- q1   # allocator reentrancy
//!   cargo run -p spike -- q2   # stable span identity (covers Q4 too)
//!   cargo run -p spike -- q3   # async propagation across worker threads
//!   cargo run -p spike -- q5   # telemetry-server binary body, end-to-end

use crate::q1::CountingAlloc;

mod q1;
mod q2;
mod q3;
mod q5;

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

fn main() {
    let arg = std::env::args().nth(1);
    match arg.as_deref() {
        Some("q1") => q1::run(),
        Some("q2") => q2::run(),
        Some("q3") => {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4)
                .enable_all()
                .build()
                .unwrap()
                .block_on(q3::run());
        }
        Some("q5") => {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap()
                .block_on(q5::run());
        }
        Some("hello") => println!("hello — foundations linked OK"),
        Some(other) => {
            eprintln!("unknown subcommand: {other}");
            std::process::exit(2);
        }
        None => {
            eprintln!("usage: spike <q1|q2|q3|q5|hello>");
            std::process::exit(2);
        }
    }
}
