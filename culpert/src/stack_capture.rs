//! Stack capture strategies — the per-sample work that lets culpert
//! later say *where* an allocation came from.
//!
//! Two implementations are available, picked at sample time via
//! [`crate::config::StackCaptureStrategy`]:
//!
//! - **[`backtrace`]-based** (the default). The `backtrace` crate
//!   delegates to the system unwinder (`libunwind`, the Apple C++
//!   unwinder, Windows' `RtlVirtualUnwind`, ...). Those unwinders walk
//!   the stack by consulting the compiler-emitted Call Frame
//!   Information tables: a per-instruction-pointer lookup tells the
//!   unwinder where the previous frame's saved registers live. Robust
//!   on every supported platform with no build-time configuration, but
//!   costs roughly hundreds of nanoseconds to several microseconds per
//!   frame.
//!
//! - **Frame-pointer-based** (opt-in via
//!   [`StackCaptureStrategy::FramePointer`](crate::config::StackCaptureStrategy::FramePointer)).
//!   Reads the frame-pointer register once, then walks the in-memory
//!   linked list of saved frame pointers. Two memory loads per frame.
//!   Requires the program to have been built with
//!   `RUSTFLAGS="-C force-frame-pointers=yes"`; otherwise the walk
//!   terminates at the first frame-pointer-less function. Implemented
//!   for x86_64 and aarch64; other targets fall back to backtrace.
//!
//! Frames are emitted as raw instruction pointers (no symbolisation).
//! Symbol resolution is deferred to snapshot time in
//! [`crate::aggregator`].

use crate::config::StackCaptureStrategy;
use smallvec::SmallVec;

/// Captured callsite as raw instruction pointers, top-of-stack first.
/// 32 inline slots — the default [`Config::stack_depth`](crate::Config::stack_depth)
/// — so a default-configured capture never spills to the heap.
pub(crate) type Frames = SmallVec<[usize; 32]>;

/// Capture the calling thread's stack as a list of instruction pointers,
/// at most `depth` frames deep, using `strategy`.
///
/// The first element is the youngest frame (closest to where the
/// allocation that triggered sampling fired). Used solely from
/// [`crate::sampler::do_observe`].
pub(crate) fn capture(strategy: StackCaptureStrategy, depth: usize) -> Frames {
    match strategy {
        StackCaptureStrategy::Backtrace => capture_backtrace(depth),
        StackCaptureStrategy::FramePointer => capture_fp_or_fallback(depth),
    }
}

// ---- Backtrace strategy ---------------------------------------------

fn capture_backtrace(depth: usize) -> Frames {
    let mut out: Frames = SmallVec::new();
    backtrace::trace(|frame| {
        if out.len() >= depth {
            return false;
        }
        out.push(frame.ip() as usize);
        true
    });
    out
}

// ---- FramePointer strategy ------------------------------------------

fn capture_fp_or_fallback(depth: usize) -> Frames {
    // Dispatch by target architecture. On x86_64 and aarch64 we have a
    // hand-written walk; everywhere else we fall back to backtrace so
    // the strategy isn't a build-time poison pill on, say, RISC-V.
    std::cfg_select! {
        target_arch = "x86_64" => x86_64::capture(depth),
        target_arch = "aarch64" => aarch64::capture(depth),
        _ => capture_backtrace(depth),
    }
}

/// Heuristic upper bound on a thread's stack size, used to gate frame
/// pointer dereferences. Tunable later; 16 MiB is generously above
/// every default thread stack size I'm aware of (Linux pthread default
/// 8 MiB, macOS main thread 8 MiB, common tokio worker stacks 2 MiB).
const STACK_RANGE_HINT_BYTES: usize = 16 * 1024 * 1024;

#[cfg(target_arch = "x86_64")]
mod x86_64 {
    use super::*;

    /// Walk the frame-pointer linked list on x86_64.
    ///
    /// The calling convention puts the saved previous-frame pointer at
    /// `[rbp]` and the return address at `[rbp + 8]`. We bootstrap by
    /// reading `rbp` into a Rust pointer via inline asm, then chase the
    /// linked list in plain Rust.
    pub(super) fn capture(depth: usize) -> Frames {
        let mut out: Frames = SmallVec::new();

        // Bootstrap: copy the current frame-pointer register into a
        // Rust variable. The `mov` is a pure register-to-register copy
        // (no memory access, no stack touch, no flag side-effects), so
        // we promise that to the optimiser.
        let mut fp: *const usize;
        unsafe {
            core::arch::asm!(
                "mov {}, rbp",
                out(reg) fp,
                options(nomem, nostack, preserves_flags),
            );
        }

        // Read the current stack pointer for the lower bound of our
        // bounds-check below. Same `mov` pattern.
        let sp: usize;
        unsafe {
            core::arch::asm!(
                "mov {}, rsp",
                out(reg) sp,
                options(nomem, nostack, preserves_flags),
            );
        }
        let stack_top_guess = sp.saturating_add(STACK_RANGE_HINT_BYTES);

        walk(&mut out, depth, fp, sp, stack_top_guess);
        out
    }
}

#[cfg(target_arch = "aarch64")]
mod aarch64 {
    use super::*;

    /// Walk the frame-pointer linked list on aarch64.
    ///
    /// Same layout as x86_64 — saved frame pointer at `[fp]`, return
    /// address at `[fp + 8]` — Apple's ABI mandates it, Linux's ABI
    /// follows. The differences from the x86_64 variant are just the
    /// register names (`x29` / `sp` instead of `rbp` / `rsp`).
    pub(super) fn capture(depth: usize) -> Frames {
        let mut out: Frames = SmallVec::new();

        let mut fp: *const usize;
        unsafe {
            core::arch::asm!(
                "mov {}, x29",
                out(reg) fp,
                options(nomem, nostack, preserves_flags),
            );
        }

        let sp: usize;
        unsafe {
            core::arch::asm!(
                "mov {}, sp",
                out(reg) sp,
                options(nomem, nostack, preserves_flags),
            );
        }
        let stack_top_guess = sp.saturating_add(STACK_RANGE_HINT_BYTES);

        walk(&mut out, depth, fp, sp, stack_top_guess);
        out
    }
}

/// Shared frame-pointer walk loop. Bootstrap (the register reads) is
/// architecture-specific; the actual chasing of the linked list is
/// portable.
///
/// `fp` must be the calling thread's current frame pointer. `sp_lower`
/// and `top_guess` are the inclusive lower and exclusive upper bounds
/// of where a legitimate frame pointer can live, in bytes. The walk
/// terminates if `fp` falls outside that range, fails to strictly
/// increase between iterations, or yields a zero return address.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
fn walk(out: &mut Frames, depth: usize, mut fp: *const usize, sp_lower: usize, top_guess: usize) {
    // Both the x86_64 and aarch64 ABIs require frame pointers (and the
    // saved-FP / return-address slots they point at) to be pointer-aligned.
    // A non-aligned value in the saved-FP slot is unambiguous "calling
    // function didn't preserve fp; the bytes here are garbage." We have to
    // detect this before dereferencing — Rust's debug-build alignment check
    // turns a misaligned `*fp` into a non-unwinding panic, which aborts the
    // worker thread when this walk runs inside the global allocator hot
    // path (e.g. `TrackingAllocator::alloc -> observe -> walk`).
    const ALIGN: usize = core::mem::align_of::<usize>();

    let mut prev_fp: usize = 0;
    while !fp.is_null() && (fp as usize) >= sp_lower && (fp as usize) < top_guess {
        // Alignment guard. Combined with the bounds check above, this is
        // what makes the SAFETY claim on the dereferences below valid.
        if !(fp as usize).is_multiple_of(ALIGN) {
            break;
        }

        // Strict-increase guard: as the walk steps outward, frame
        // pointers must move toward higher addresses (stacks grow
        // down). If the chain ever turns around, the data is corrupt
        // and we stop rather than chase garbage.
        if (fp as usize) <= prev_fp {
            break;
        }
        prev_fp = fp as usize;

        // SAFETY: we just verified `fp` is in the plausible-stack range
        // AND pointer-aligned. The cell at `[fp + 8]` is, by calling
        // convention, the return address saved at this frame's entry.
        // The worst remaining failure mode (frame pointers omitted on
        // some caller, garbage value in the saved slot) is "we record a
        // bogus IP and bail next iteration when the bounds + alignment
        // check catches the next fp" — not a segfault, not an abort.
        let return_addr = unsafe { *fp.add(1) };
        if return_addr == 0 {
            break;
        }
        out.push(return_addr);
        if out.len() >= depth {
            break;
        }

        // SAFETY: same justification — `[fp]` is the saved previous
        // frame pointer. The very next iteration's bounds + alignment
        // checks catch the case where this value is not actually a
        // valid stack address.
        fp = unsafe { *fp as *const usize };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper that intentionally lives below `outer` in the call stack
    /// so a successful frame-pointer walk should see at least two
    /// frames (this one and `outer`).
    #[inline(never)]
    fn inner() -> (Frames, Frames) {
        let bt = capture(StackCaptureStrategy::Backtrace, 16);
        let fp = capture(StackCaptureStrategy::FramePointer, 16);
        (bt, fp)
    }

    #[inline(never)]
    fn outer() -> (Frames, Frames) {
        inner()
    }

    /// Loose smoke test. We avoid asserting exact frame contents because
    /// the test runner's RUSTFLAGS may or may not include
    /// `-C force-frame-pointers=yes`, and even with it the optimiser
    /// can inline `inner` / `outer`. We assert:
    ///
    /// - Neither capture panics or segfaults.
    /// - The backtrace strategy always produces ≥ 1 frame (it works
    ///   regardless of frame-pointer availability).
    /// - The frame-pointer strategy produces ≥ 0 frames — i.e. we
    ///   terminate cleanly even if the build has no frame pointers.
    #[test]
    fn captures_terminate_cleanly_on_either_strategy() {
        let (bt, fp) = outer();
        assert!(
            !bt.is_empty(),
            "backtrace strategy should always produce at least one frame"
        );
        // Frame-pointer strategy: just check we didn't panic or segfault
        // (we got here). Frame count depends on build flags.
        assert!(fp.len() <= 16);
    }

    /// Regression test for the alignment-guard bug: if the saved-FP slot
    /// of a stack frame holds a value that's in-range but misaligned (the
    /// real-world cause: a calling function that doesn't preserve frame
    /// pointers and happens to leave non-aligned bytes in `rbp`), the
    /// walker used to dereference it and trigger a non-unwinding panic
    /// (`misaligned pointer dereference: address must be a multiple of 0x8`).
    ///
    /// That was fatal in practice because `walk` runs inside the global
    /// allocator hot path, so the abort took out the calling thread.
    ///
    /// The fix is the `(fp as usize) % ALIGN != 0` early-break above.
    /// This test constructs a forged "stack" where the saved-FP slot of
    /// the first frame contains a misaligned in-range value and verifies
    /// `walk` returns cleanly with whatever frames it accumulated before
    /// hitting the bad pointer.
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn walk_bails_on_misaligned_fp_in_saved_slot() {
        // Layout: two adjacent "frames" in a Vec<usize>:
        //
        //   [0] = misaligned next-fp value         (saved fp from caller)
        //   [1] = 0xDEADBEEF                       (saved ra from caller)
        //
        // We pass `&buf[0]` as the initial fp. The walker reads ra at
        // `fp.add(1)` (ok — buf is aligned), records 0xDEADBEEF, then
        // reads next_fp at `*fp` (a misaligned address). Before the fix
        // this panicked. After the fix it must break cleanly.
        let mut buf = [0_usize; 4];

        // Pick a misaligned value that falls inside `buf` so the bounds
        // check passes but the alignment check fails. `&buf[2]` is
        // aligned; `&buf[2] + 3` is in-range but 3 bytes off-alignment.
        let misaligned = (&buf[2] as *const usize as usize) + 3;
        debug_assert_ne!(misaligned % core::mem::align_of::<usize>(), 0);

        buf[0] = misaligned;
        buf[1] = 0xDEAD_BEEF;

        let fp = &buf[0] as *const usize;
        let sp_lower = buf.as_ptr() as usize;
        let top_guess = sp_lower + buf.len() * core::mem::size_of::<usize>() + 1;

        let mut frames = Frames::default();
        // The walk MUST NOT panic. If the alignment guard is missing,
        // the second iteration deref of the misaligned pointer will
        // abort the test process.
        walk(&mut frames, 16, fp, sp_lower, top_guess);

        // We expect exactly one frame captured (the ra at buf[1]) before
        // the walker followed the misaligned next-fp and bailed.
        assert_eq!(frames.len(), 1, "frames captured before bailing");
        assert_eq!(frames.as_slice()[0], 0xDEAD_BEEF, "captured ra is buf[1]");
    }
}
