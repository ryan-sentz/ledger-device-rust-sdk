//! Demonstrates the non-blocking `NbglSpinnerPage` API.
//!
//! Unlike `NbglSpinner` (which wraps `nbgl_useCaseSpinner`), this is drawn on
//! the page layer, so it coexists with `NbglPage` and the blocking widgets.
//! `draw()` returns immediately; the application drives the animation with
//! `tick()` and can change the text at any time with `update()`.
//!
//! Run under Speculos with:
//!
//! ```bash
//! cargo run --example nbgl_spinner_page --target flex --release \
//!     --features io_new --config ledger_device_sdk/examples/config.toml
//! ```

#![no_std]
#![no_main]

ledger_device_sdk::set_panic!(ledger_device_sdk::exiting_panic);
ledger_device_sdk::define_comm!(COMM);

/// The page API is gated behind the C SDK's `NBGL_PAGE` define, which is set
/// for touchscreen devices only. There is nothing to demonstrate on Nano.
#[cfg(any(target_os = "stax", target_os = "flex", target_os = "apex_p"))]
#[unsafe(no_mangle)]
extern "C" fn sample_main() {
    use ledger_device_sdk::nbgl::{NbglSpinnerPage, init_comm};

    /// Ticker events between percentage updates (the ticker runs at 100ms).
    const TICKS_PER_STEP: u32 = 8;

    let comm = init_comm(&COMM);

    let mut page = NbglSpinnerPage::new();
    page.draw("Signing Transaction", "0%").unwrap();

    let mut ticks = 0u32;
    let mut percent = 0u8;

    loop {
        // Must be `next_event`, not `next_command`: the latter blocks until an
        // APDU arrives and would never observe the ticker.
        let _evt = comm.next_event();

        ticks = ticks.wrapping_add(1);
        if ticks % TICKS_PER_STEP != 0 {
            continue;
        }

        // The spinner turns on NBGL's own 400ms ticker; just walk the
        // percentage to show text updates.
        percent = (percent + 5) % 105;
        let mut buff = [0u8; 8];
        let text = fmt_percent(&mut buff, percent);
        page.update("Signing Transaction", text);

        // A blocking widget or another page would have taken over the single
        // non-modal layout; redraw when that happens.
        if !page.is_live() {
            page.draw("Signing Transaction", text).unwrap();
        }
    }
}

/// Formats `v` as "NN%" without allocating.
#[cfg(any(target_os = "stax", target_os = "flex", target_os = "apex_p"))]
fn fmt_percent(buff: &mut [u8; 8], v: u8) -> &str {
    let mut n = 0;
    if v >= 100 {
        buff[n] = b'0' + v / 100;
        n += 1;
    }
    if v >= 10 {
        buff[n] = b'0' + (v / 10) % 10;
        n += 1;
    }
    buff[n] = b'0' + v % 10;
    n += 1;
    buff[n] = b'%';
    n += 1;

    core::str::from_utf8(&buff[..n]).unwrap()
}

#[cfg(any(target_os = "nanosplus", target_os = "nanox"))]
#[unsafe(no_mangle)]
extern "C" fn sample_main() {}
