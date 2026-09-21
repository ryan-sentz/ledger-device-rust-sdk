//! Demonstrates the non-blocking `NbglPage` API.
//!
//! Unlike the `nbgl_useCase*` widgets, `draw()` returns immediately: the page
//! stays on screen while the application keeps pumping its own event loop, and
//! touch events are collected with `take_event()`.
//!
//! Run under Speculos with:
//!
//! ```bash
//! cargo run --example nbgl_page --target stax --release \
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
    use ledger_device_sdk::nbgl::{
        Field, NbglPage, NbglPageContent, NbglPageNav, TOKEN_FIRST_FREE, TagValueList, TuneIndex,
        init_comm,
    };

    const TOKEN_QUIT: u8 = TOKEN_FIRST_FREE;
    const TOKEN_NAV: u8 = TOKEN_FIRST_FREE + 1;

    let comm = init_comm(&COMM);

    let fields = [
        Field {
            name: "Amount",
            value: "111 CRAB",
        },
        Field {
            name: "Destination",
            value: "0x1234567890ABCDEF",
        },
    ];

    let mut page = NbglPage::new(NbglPageContent::TagValueList(TagValueList::new(
        &fields, 2, false, true,
    )))
    .title("Review transaction")
    .tune(TuneIndex::TapCasual)
    // A clean refresh for the first paint avoids e-ink artifacts.
    .clean_refresh(true)
    .nav(
        NbglPageNav::with_buttons()
            .pages(0, 2)
            .quit("Reject", TOKEN_QUIT),
    );

    page.draw().unwrap();

    loop {
        // `next_event` calls `ux_process_finger_event` before returning, so the
        // NBGL callback has already run and `take_event` has the result. Note
        // this must be `next_event`, not `next_command`: the latter blocks
        // until an APDU arrives and would not observe touches.
        let _evt = comm.next_event();

        if let Some(e) = page.take_event() {
            match e.token {
                TOKEN_QUIT => {
                    ledger_device_sdk::log::debug!("quit");
                    ledger_device_sdk::exit_app(0);
                }
                // Index is the new active page.
                TOKEN_NAV => ledger_device_sdk::log::debug!("nav index={}", e.index),
                t => ledger_device_sdk::log::debug!("token={} index={}", t, e.index),
            }
        }

        // A blocking widget or another page would have taken over the single
        // non-modal layout; redraw when that happens.
        if !page.is_live() {
            page.draw().unwrap();
        }
    }
}

#[cfg(any(target_os = "nanosplus", target_os = "nanox"))]
#[unsafe(no_mangle)]
extern "C" fn sample_main() {}
