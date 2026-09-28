//! A safe wrapper around the NBGL `nbgl_page_t` / `nbgl_pageContent_t` C API.
//!
//! Every other widget in this module is built on the `nbgl_useCase*` layer and
//! *blocks*: `show()` spins until a C callback signals completion. `NbglPage`
//! wraps the layer underneath — [`nbgl_pageDrawGenericContent`] — which paints a
//! screen and returns immediately, reporting user interaction as tokens.
//!
//! That makes it the right tool when the application must stay responsive while
//! a screen is displayed, for example to keep serving APDUs, or to put up a
//! screen from inside an NBGL callback (where calling a blocking widget is
//! forbidden).
//!
//! A blocking [`NbglPage::show`] is also provided for callers that just want the
//! conventional ergonomics.
//!
//! Only available on touchscreen devices: `nbgl_page.h` is gated behind the C
//! `NBGL_PAGE` define, which the SDK sets for stax, flex and apex_p only.
//!
//! # Example
//!
//! ```rust,ignore
//! let mut page = NbglPage::new(NbglPageContent::TagValueList(list))
//!     .title("Review")
//!     .nav(NbglPageNav::with_buttons().pages(0, 2).quit("Reject", 60));
//!
//! page.draw()?;
//!
//! loop {
//!     let _evt: Event<ApduHeader> = comm.next_event();
//!     if let Some(e) = page.take_event() {
//!         // react to e.token / e.index
//!     }
//!     if !page.is_live() {
//!         page.draw()?;
//!     }
//! }
//! ```

use super::*;
use alloc::boxed::Box;
use core::sync::atomic::{AtomicU8, AtomicU32, Ordering};

/// Token reported for the confirm button of a
/// [`TagValueConfirm`](super::TagValueConfirm).
pub const TOKEN_CONFIRM: u8 = FIRST_USER_TOKEN as u8;
/// Token reported for the cancel button of a
/// [`TagValueConfirm`](super::TagValueConfirm).
pub const TOKEN_CANCEL: u8 = FIRST_USER_TOKEN as u8 + 1;
/// Token reported for the details button of a
/// [`TagValueConfirm`](super::TagValueConfirm).
pub const TOKEN_DETAILS: u8 = FIRST_USER_TOKEN as u8 + 2;
/// First token not used by any built-in content element. Application-chosen
/// tokens for titles, top-right buttons and navigation should start here so
/// they cannot collide with the content tokens above.
pub const TOKEN_FIRST_FREE: u8 = FIRST_USER_TOKEN as u8 + 8;

/// Most recent touch event, packed as `1 << 16 | (token << 8) | index`.
/// Zero means "nothing pending" (bit 16 is the presence flag, so a genuine
/// `token == 0, index == 0` event is still distinguishable).
static PAGE_EVENT: AtomicU32 = AtomicU32::new(0);

/// Bumped whenever something takes over the single background layout.
///
/// NBGL keeps exactly one non-modal layout (`gLayout[0]` in `nbgl_layout.c`);
/// `nbgl_layoutGet` returns it for every non-modal request and `memset`s it.
/// So every non-modal page handle is the *same pointer* and cannot be used to
/// tell pages apart. This counter is how an [`NbglPage`] knows whether it still
/// owns the screen, and therefore whether releasing its handle is safe.
pub(crate) static PAGE_GENERATION: AtomicU32 = AtomicU32::new(0);

/// Number of modal pages currently drawn.
static MODAL_COUNT: AtomicU8 = AtomicU8::new(0);

/// NBGL has `NB_MAX_LAYOUTS - 1` modal slots (`NB_MAX_LAYOUTS` is 3).
const MAX_MODALS: u8 = 2;

const EVENT_PRESENT: u32 = 1 << 16;

/// Records a touch event for later collection. Deliberately does *not* touch
/// the `G_RET` / `G_ENDED` globals, so a live page cannot disturb the state
/// machine of the blocking widgets.
unsafe extern "C" fn page_touch_callback(token: c_int, index: u8) {
    PAGE_EVENT.store(
        EVENT_PRESENT | ((token as u32 & 0xff) << 8) | index as u32,
        Ordering::Release,
    );
}

/// As [`page_touch_callback`], but also ends the [`SyncNBGL`] wait loop. Used
/// only by the blocking [`NbglPage::show`].
unsafe extern "C" fn page_touch_callback_sync(token: c_int, index: u8) {
    unsafe {
        page_touch_callback(token, index);
        G_ENDED = true;
    }
}

fn decode_event(raw: u32) -> Option<NbglPageEvent> {
    (raw & EVENT_PRESENT != 0).then_some(NbglPageEvent {
        token: ((raw >> 8) & 0xff) as u8,
        index: (raw & 0xff) as u8,
    })
}

/// A user interaction reported by a drawn [`NbglPage`].
///
/// `token` identifies the control that was touched. Content elements use the
/// fixed [`TOKEN_CONFIRM`] / [`TOKEN_CANCEL`] / [`TOKEN_DETAILS`] values; title,
/// top-right and navigation tokens are chosen by the caller.
///
/// `index` disambiguates controls that share a token — notably the navigation
/// bar, where it is the new zero-based active page after the arrow was
/// touched (NBGL sets it from `layout->activePage`), not a back / forward
/// direction.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct NbglPageEvent {
    /// Token of the touched control.
    pub token: u8,
    /// Sub-index within the control, where applicable.
    pub index: u8,
}

/// Errors returned by [`NbglPage`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum NbglPageError {
    /// `nbgl_pageDrawGenericContent` returned NULL.
    DrawFailed,
    /// Two modal pages are already drawn.
    ///
    /// NBGL only has two modal layout slots, and
    /// `nbgl_pageDrawGenericContentExt` does not check `nbgl_layoutGet` for
    /// NULL before using its result — it would dereference a null pointer
    /// rather than fail. So this is checked before calling into C.
    TooManyModals,
    /// [`NbglPage::show`] was asked to exit on APDU and one arrived before the
    /// user touched anything.
    ApduReceived,
    /// The blocking wait ended without a recorded touch event.
    NoEvent,
}

/// How an [`NbglPage`] lets the user move between pages.
pub enum NbglPageNavStyle {
    /// Forward by tapping the main area, with an optional back arrow
    /// (`NAV_WITH_TAP`).
    Tap {
        /// Show a back arrow in the top-left.
        back_button: bool,
        /// Token reported when the back arrow is touched.
        back_token: u8,
        /// Token reported when the main area is tapped.
        next_page_token: u8,
        /// Text hinting that the main area is tappable.
        next_page_text: Option<CString>,
        /// Footer text; `None` for no footer.
        quit_text: Option<CString>,
    },
    /// Forward and backward via arrows in a bottom navigation bar
    /// (`NAV_WITH_BUTTONS`).
    Buttons {
        /// Show a quit control in the navigation bar.
        quit_button: bool,
        /// Show a back arrow.
        back_button: bool,
        /// Show the "n of m" page indicator.
        visible_page_indicator: bool,
        /// Token reported for navigation; index is the new active page.
        nav_token: u8,
        /// Footer text shown beside the arrows.
        quit_text: Option<CString>,
    },
}

/// Navigation controls attached to an [`NbglPage`].
///
/// Owns its strings, because NBGL stores the pointers rather than copying.
///
/// The C layer's behaviour is worth knowing, as several combinations render
/// nothing (`nbgl_page.c`):
///
/// - [`NbglPageNavStyle::Tap`]: with no skip text you get a plain footer; with
///   skip text the footer is split to carry a Skip control.
/// - [`NbglPageNavStyle::Buttons`]: with `nb_pages <= 1` **and** no quit text,
///   no footer is drawn at all.
/// - `progress_indicator` is honoured on stax only; flex and apex_p ignore it.
pub struct NbglPageNav {
    active_page: u8,
    nb_pages: u8,
    quit_token: u8,
    progress_indicator: bool,
    tune_id: TuneIndex,
    skip_text: Option<CString>,
    skip_token: u8,
    style: NbglPageNavStyle,
}

impl NbglPageNav {
    /// Navigation by tapping the main area (`NAV_WITH_TAP`).
    #[must_use]
    pub fn with_tap() -> NbglPageNav {
        NbglPageNav {
            active_page: 0,
            nb_pages: 1,
            quit_token: TOKEN_FIRST_FREE,
            progress_indicator: false,
            tune_id: TuneIndex::TapCasual,
            skip_text: None,
            skip_token: TOKEN_FIRST_FREE + 1,
            style: NbglPageNavStyle::Tap {
                back_button: false,
                back_token: TOKEN_FIRST_FREE + 2,
                next_page_token: TOKEN_FIRST_FREE + 3,
                next_page_text: None,
                quit_text: None,
            },
        }
    }

    /// Navigation via a bottom button bar (`NAV_WITH_BUTTONS`).
    #[must_use]
    pub fn with_buttons() -> NbglPageNav {
        NbglPageNav {
            active_page: 0,
            nb_pages: 1,
            quit_token: TOKEN_FIRST_FREE,
            progress_indicator: false,
            tune_id: TuneIndex::TapCasual,
            skip_text: None,
            skip_token: TOKEN_FIRST_FREE + 1,
            style: NbglPageNavStyle::Buttons {
                quit_button: true,
                back_button: true,
                visible_page_indicator: true,
                nav_token: TOKEN_FIRST_FREE + 4,
                quit_text: None,
            },
        }
    }

    /// Sets the zero-based active page and the total page count.
    #[must_use]
    pub fn pages(self, active: u8, total: u8) -> NbglPageNav {
        NbglPageNav {
            active_page: active,
            nb_pages: total,
            ..self
        }
    }

    /// Sets the footer / quit text and the token it reports.
    #[must_use]
    pub fn quit(self, text: &str, token: u8) -> NbglPageNav {
        let quit = Some(CString::new(text).unwrap());
        let style = match self.style {
            NbglPageNavStyle::Tap {
                back_button,
                back_token,
                next_page_token,
                next_page_text,
                ..
            } => NbglPageNavStyle::Tap {
                back_button,
                back_token,
                next_page_token,
                next_page_text,
                quit_text: quit,
            },
            NbglPageNavStyle::Buttons {
                quit_button,
                back_button,
                visible_page_indicator,
                nav_token,
                ..
            } => NbglPageNavStyle::Buttons {
                quit_button,
                back_button,
                visible_page_indicator,
                nav_token,
                quit_text: quit,
            },
        };
        NbglPageNav {
            quit_token: token,
            style,
            ..self
        }
    }

    /// Sets the token reported by the navigation arrows of a
    /// [`with_buttons`](NbglPageNav::with_buttons) bar, whose event index is
    /// the new active page. Ignored for [`with_tap`](NbglPageNav::with_tap).
    #[must_use]
    pub fn nav_token(self, token: u8) -> NbglPageNav {
        let style = match self.style {
            NbglPageNavStyle::Buttons {
                quit_button,
                back_button,
                visible_page_indicator,
                quit_text,
                ..
            } => NbglPageNavStyle::Buttons {
                quit_button,
                back_button,
                visible_page_indicator,
                nav_token: token,
                quit_text,
            },
            tap => tap,
        };
        NbglPageNav { style, ..self }
    }

    /// Adds a Skip control to the footer.
    #[must_use]
    pub fn skip(self, text: &str, token: u8) -> NbglPageNav {
        NbglPageNav {
            skip_text: Some(CString::new(text).unwrap()),
            skip_token: token,
            ..self
        }
    }

    /// Enables the top progress indicator. Ignored on flex and apex_p.
    #[must_use]
    pub fn progress_indicator(self, enabled: bool) -> NbglPageNav {
        NbglPageNav {
            progress_indicator: enabled,
            ..self
        }
    }

    /// Sets the tune played when a navigation control is touched.
    #[must_use]
    pub fn tune(self, tune_id: TuneIndex) -> NbglPageNav {
        NbglPageNav { tune_id, ..self }
    }
}

#[allow(clippy::needless_update)]
impl From<&NbglPageNav> for nbgl_pageNavigationInfo_t {
    fn from(nav: &NbglPageNav) -> nbgl_pageNavigationInfo_t {
        let (nav_type, union) = match &nav.style {
            NbglPageNavStyle::Tap {
                back_button,
                back_token,
                next_page_token,
                next_page_text,
                quit_text,
            } => (
                NAV_WITH_TAP,
                nbgl_pageMultiScreensDescription_s__bindgen_ty_1 {
                    navWithTap: nbgl_pageNavWithTap_t {
                        backButton: *back_button,
                        backToken: *back_token,
                        nextPageToken: *next_page_token,
                        nextPageText: next_page_text
                            .as_ref()
                            .map_or(core::ptr::null(), |t| t.as_ptr() as *const c_char),
                        quitText: quit_text
                            .as_ref()
                            .map_or(core::ptr::null(), |t| t.as_ptr() as *const c_char),
                    },
                },
            ),
            NbglPageNavStyle::Buttons {
                quit_button,
                back_button,
                visible_page_indicator,
                nav_token,
                quit_text,
            } => (
                NAV_WITH_BUTTONS,
                nbgl_pageMultiScreensDescription_s__bindgen_ty_1 {
                    navWithButtons: nbgl_pageNavWithButtons_t {
                        quitButton: *quit_button,
                        backButton: *back_button,
                        visiblePageIndicator: *visible_page_indicator,
                        navToken: *nav_token,
                        quitText: quit_text
                            .as_ref()
                            .map_or(core::ptr::null(), |t| t.as_ptr() as *const c_char),
                    },
                },
            ),
        };

        nbgl_pageNavigationInfo_t {
            activePage: nav.active_page,
            nbPages: nav.nb_pages,
            quitToken: nav.quit_token,
            navType: nav_type,
            progressIndicator: nav.progress_indicator,
            tuneId: nav.tune_id as u8,
            skipText: nav
                .skip_text
                .as_ref()
                .map_or(core::ptr::null(), |t| t.as_ptr() as *const c_char),
            skipToken: nav.skip_token,
            __bindgen_anon_1: union,
            ..Default::default()
        }
    }
}

/// A single NBGL page, drawn directly via `nbgl_pageDrawGenericContent`.
///
/// [`draw`](NbglPage::draw) returns as soon as the page is on screen. Touch
/// events are delivered by the C layer while the application pumps its own
/// event loop, and are collected with [`take_event`](NbglPage::take_event).
///
/// # Lifetime
///
/// NBGL does not copy the strings and icons it is given; it stores the raw
/// pointers in its object tree. `NbglPage` therefore owns everything the C
/// layer refers to, and **must stay alive for as long as the page is
/// displayed**. [`Drop`] releases the page, so keeping the value in the
/// application's UI state is the intended usage.
///
/// Moving an `NbglPage` is safe: every pointer handed to C targets a heap
/// allocation (`CString`, `Vec`, boxed icons), not the struct itself.
///
/// # One screen at a time
///
/// NBGL keeps a single non-modal layout. Drawing another page, or calling any
/// blocking widget, or `NbglHomeAndSettings::show_and_return`, replaces a live
/// page. Use [`is_live`](NbglPage::is_live) to detect that and
/// [`draw`](NbglPage::draw) again to restore it. A displaced page's `Drop` is a
/// no-op rather than releasing a layout it no longer owns.
///
/// For a transient overlay that stacks on top of the current screen, use
/// [`modal`](NbglPage::modal) instead — at most two may be live at once.
pub struct NbglPage {
    // Owned data the C layer points into; must outlive the drawn page.
    content: NbglPageContent,
    title: Option<CString>,
    top_right_icon: Option<Box<nbgl_icon_details_t>>,
    nav: Option<NbglPageNav>,

    // Configuration.
    title_token: u8,
    top_right_token: u8,
    touchable_title: bool,
    tune_id: TuneIndex,
    modal: bool,
    refresh_mode: nbgl_refresh_mode_t,

    // Runtime state.
    /// Handle from `nbgl_pageDrawGenericContent*`; null when not drawn.
    handle: *mut nbgl_page_t,
    /// Value of [`PAGE_GENERATION`] at draw time, for non-modal pages.
    generation: u32,
}

impl NbglPage {
    /// Creates a page showing `content`.
    pub fn new(content: NbglPageContent) -> NbglPage {
        NbglPage {
            content,
            title: None,
            top_right_icon: None,
            nav: None,
            title_token: TOKEN_FIRST_FREE + 5,
            top_right_token: TOKEN_FIRST_FREE + 6,
            touchable_title: false,
            tune_id: TuneIndex::TapCasual,
            modal: false,
            refresh_mode: FULL_COLOR_PARTIAL_REFRESH,
            handle: core::ptr::null_mut(),
            generation: 0,
        }
    }

    /// Sets the page title. Without one, no header is drawn.
    #[must_use]
    pub fn title(mut self, title: &str) -> NbglPage {
        self.title = Some(CString::new(title).unwrap());
        self
    }

    /// Makes the title a tappable back-header reporting `token`.
    #[must_use]
    pub fn touchable_title(mut self, token: u8) -> NbglPage {
        self.touchable_title = true;
        self.title_token = token;
        self
    }

    /// Adds a top-right button with the given icon, reporting `token`.
    #[must_use]
    pub fn top_right_icon(mut self, glyph: &NbglGlyph, token: u8) -> NbglPage {
        self.top_right_icon = Some(Box::new(glyph.into()));
        self.top_right_token = token;
        self
    }

    /// Sets the tune played when the title or top-right button is touched.
    #[must_use]
    pub fn tune(mut self, tune_id: TuneIndex) -> NbglPage {
        self.tune_id = tune_id;
        self
    }

    /// Attaches navigation controls.
    #[must_use]
    pub fn nav(mut self, nav: NbglPageNav) -> NbglPage {
        self.nav = Some(nav);
        self
    }

    /// Draws this page as a modal on top of the current screen.
    ///
    /// At most two modal pages may be live at once; a third
    /// [`draw`](NbglPage::draw) returns [`NbglPageError::TooManyModals`].
    #[must_use]
    pub fn modal(mut self, modal: bool) -> NbglPage {
        self.modal = modal;
        self
    }

    /// Uses a full clean refresh instead of the default partial refresh.
    ///
    /// Worth enabling for the first page drawn after a screen change, where a
    /// partial refresh can leave artifacts on e-ink.
    #[must_use]
    pub fn clean_refresh(mut self, clean: bool) -> NbglPage {
        self.refresh_mode = if clean {
            FULL_COLOR_CLEAN_REFRESH
        } else {
            FULL_COLOR_PARTIAL_REFRESH
        };
        self
    }

    /// Draws the page and returns immediately.
    ///
    /// Any previously drawn instance of this page is released first. Call
    /// [`take_event`](NbglPage::take_event) from the application's event loop
    /// to collect user interaction.
    pub fn draw(&mut self) -> Result<(), NbglPageError> {
        self.draw_with(Some(page_touch_callback))
    }

    fn draw_with(&mut self, callback: nbgl_layoutTouchCallback_t) -> Result<(), NbglPageError> {
        self.release();

        // `nbgl_pageDrawGenericContentExt` passes the result of
        // `nbgl_layoutGet` straight to `nbgl_layoutAdd*` without a NULL check,
        // so an exhausted modal pool must be caught here rather than detected
        // from the return value.
        if self.modal && MODAL_COUNT.load(Ordering::Acquire) >= MAX_MODALS {
            return Err(NbglPageError::TooManyModals);
        }

        let mut c_content: nbgl_pageContent_t = (&self.content).into();
        c_content.title = self
            .title
            .as_ref()
            .map_or(core::ptr::null(), |t| t.as_ptr() as *const c_char);
        c_content.isTouchableTitle = self.touchable_title;
        c_content.titleToken = self.title_token;
        c_content.tuneId = self.tune_id as u8;
        c_content.topRightToken = self.top_right_token;
        c_content.topRightIcon = self
            .top_right_icon
            .as_deref()
            .map_or(core::ptr::null(), |i| i as *const nbgl_icon_details_t);

        let c_nav = self.nav.as_ref().map(nbgl_pageNavigationInfo_t::from);
        let nav_ptr = c_nav
            .as_ref()
            .map_or(core::ptr::null(), |n| n as *const nbgl_pageNavigationInfo_t);

        PAGE_EVENT.store(0, Ordering::Release);

        // SAFETY: `c_content` and `c_nav` are only read for the duration of the
        // call. Every pointer they carry targets a heap allocation owned by
        // `self`, which outlives the drawn page because `Drop` releases it.
        let handle = unsafe {
            nbgl_pageDrawGenericContentExt(
                callback,
                nav_ptr,
                &mut c_content as *mut nbgl_pageContent_t,
                self.modal,
            )
        };

        if handle.is_null() {
            return Err(NbglPageError::DrawFailed);
        }

        // `nbgl_pageDrawGenericContent` ends at `nbgl_layoutDraw`, which only
        // re-renders into the framebuffer. Without an explicit refresh the
        // screen does not change.
        unsafe { nbgl_refreshSpecial(self.refresh_mode) };

        self.handle = handle;
        if self.modal {
            MODAL_COUNT.fetch_add(1, Ordering::AcqRel);
        } else {
            self.generation = PAGE_GENERATION
                .fetch_add(1, Ordering::AcqRel)
                .wrapping_add(1);
        }
        Ok(())
    }

    /// Returns and clears the pending touch event, if any.
    ///
    /// Call once per event-loop turn. Under `io_legacy` the NBGL callback runs
    /// inside `ux_process_finger_event`, before `Event::TouchEvent` reaches the
    /// application, so the event is already recorded when `next_event` returns.
    pub fn take_event(&mut self) -> Option<NbglPageEvent> {
        decode_event(PAGE_EVENT.swap(0, Ordering::AcqRel))
    }

    /// Returns the pending touch event without clearing it.
    pub fn peek_event(&self) -> Option<NbglPageEvent> {
        decode_event(PAGE_EVENT.load(Ordering::Acquire))
    }

    /// Whether this page still owns the screen.
    ///
    /// Becomes `false` once another page or a blocking widget has drawn over
    /// it, at which point [`draw`](NbglPage::draw) restores it.
    pub fn is_live(&self) -> bool {
        !self.handle.is_null()
            && (self.modal || PAGE_GENERATION.load(Ordering::Acquire) == self.generation)
    }

    /// Releases the page. Idempotent, and called automatically by [`Drop`].
    ///
    /// Modal pages should be released in reverse order of drawing, since NBGL
    /// pops screen layers.
    pub fn release(&mut self) {
        if self.handle.is_null() {
            return;
        }

        if self.modal {
            // SAFETY: `handle` came from a successful modal draw and has not
            // been released yet. The redraw restores whatever was underneath.
            unsafe {
                nbgl_pageRelease(self.handle);
                nbgl_screenRedraw();
                nbgl_refresh();
            }
            MODAL_COUNT.fetch_sub(1, Ordering::AcqRel);
        } else if PAGE_GENERATION.load(Ordering::Acquire) == self.generation {
            // Only release while we still own the single background layout;
            // otherwise the handle now refers to someone else's layout.
            //
            // SAFETY: `handle` came from a successful draw, has not been
            // released, and the generation check confirms it is still ours.
            unsafe { nbgl_pageRelease(self.handle) };
            // No redraw: the caller decides what replaces this page.
        }

        self.handle = core::ptr::null_mut();
    }
}

impl Drop for NbglPage {
    fn drop(&mut self) {
        self.release();
    }
}

impl SyncNBGL for NbglPage {}

impl NbglPage {
    fn show_internal(&mut self, exit_on_apdu: bool) -> Result<NbglPageEvent, NbglPageError> {
        // Order matters. `ux_sync_init` bumps `PAGE_GENERATION`, after which this
        // page would consider itself displaced and skip releasing a handle it
        // still owns — so release first, while the generation still matches.
        // `draw_with` re-records the generation, so `is_live()` stays correct.
        self.release();
        self.ux_sync_init();
        self.draw_with(Some(page_touch_callback_sync))?;

        match self.ux_sync_wait(exit_on_apdu) {
            SyncNbgl::UxSyncRetApduReceived => Err(NbglPageError::ApduReceived),
            _ => self.take_event().ok_or(NbglPageError::NoEvent),
        }
    }

    /// Draws the page and blocks until the user touches a control.
    ///
    /// The page is left on screen; drop it or draw the next screen when done.
    #[cfg(feature = "io_new")]
    pub fn show<const N: usize>(
        &mut self,
        _comm: &mut crate::io::Comm<N>,
    ) -> Result<NbglPageEvent, NbglPageError> {
        self.show_internal(false)
    }

    /// Draws the page and blocks until the user touches a control.
    ///
    /// The page is left on screen; drop it or draw the next screen when done.
    #[cfg(not(feature = "io_new"))]
    pub fn show(&mut self) -> Result<NbglPageEvent, NbglPageError> {
        self.show_internal(false)
    }

    /// As [`show`](NbglPage::show), but returns
    /// [`NbglPageError::ApduReceived`] if an APDU arrives first.
    #[cfg(feature = "io_new")]
    pub fn show_or_apdu<const N: usize>(
        &mut self,
        _comm: &mut crate::io::Comm<N>,
    ) -> Result<NbglPageEvent, NbglPageError> {
        self.show_internal(true)
    }

    /// As [`show`](NbglPage::show), but returns
    /// [`NbglPageError::ApduReceived`] if an APDU arrives first.
    #[cfg(not(feature = "io_new"))]
    pub fn show_or_apdu(&mut self) -> Result<NbglPageEvent, NbglPageError> {
        self.show_internal(true)
    }
}

/// A non-blocking spinner page, drawn directly onto an NBGL layout.
///
/// [`NbglSpinner`](super::NbglSpinner) wraps `nbgl_useCaseSpinner`, which owns
/// the `useCase` generic context and so cannot be mixed with [`NbglPage`].
/// This is the page-layer equivalent: it builds the same spinner widget with
/// [`nbgl_layoutAddSpinner`] and participates in the [`PAGE_GENERATION`]
/// bookkeeping, so it coexists with the other pages and the blocking widgets.
///
/// It is built on the layout calls rather than `nbgl_pageDrawSpinner` because
/// that function passes a null `subText`, and [`nbgl_layoutUpdateSpinner`] then
/// refuses later subText updates — it requires the spinner container to have
/// been built with three children. A spinner that should ever show a subText
/// must therefore be created with one.
///
/// # Lifetime
///
/// As with [`NbglPage`], NBGL stores the string pointers it is given rather
/// than copying, so this value must stay alive for as long as the spinner is
/// displayed.
///
/// # Animation
///
/// The spinner turns by itself: `nbgl_layoutAddSpinner` registers a 400ms
/// ticker with NBGL, which advances it for as long as the application's event
/// loop keeps running. [`update`](NbglSpinnerPage::update) also advances it,
/// matching `nbgl_useCaseSpinner`. [`tick`](NbglSpinnerPage::tick) is only
/// needed to drive the animation manually, and most callers do not need it.
pub struct NbglSpinnerPage {
    /// Double-buffered so an update always presents a *different* pointer to
    /// NBGL; see [`update`](NbglSpinnerPage::update).
    text: [CString; 2],
    sub_text: [CString; 2],
    idx: usize,

    /// Current spinner position, in `0..NB_SPINNER_POSITIONS`.
    position: u8,

    /// Handle from `nbgl_layoutGet`; null when not drawn.
    handle: *mut nbgl_layout_t,
    /// Value of [`PAGE_GENERATION`] at draw time.
    generation: u32,
}

impl Default for NbglSpinnerPage {
    fn default() -> Self {
        Self::new()
    }
}

impl NbglSpinnerPage {
    /// Creates a new spinner page. Nothing is drawn until
    /// [`draw`](NbglSpinnerPage::draw) is called.
    pub fn new() -> NbglSpinnerPage {
        NbglSpinnerPage {
            text: [CString::default(), CString::default()],
            sub_text: [CString::default(), CString::default()],
            idx: 0,
            position: 0,
            handle: core::ptr::null_mut(),
            generation: 0,
        }
    }

    /// Stores `text` / `sub_text` in the buffer slot NBGL is not currently
    /// reading, and returns pointers to them.
    ///
    /// NBGL detects a text change by comparing the bytes behind the new pointer
    /// with the bytes behind the one it holds. Rewriting the live buffer in
    /// place would always compare equal, so the screen would never be redrawn —
    /// hence the flip.
    fn stage(&mut self, text: &str, sub_text: &str) -> (*const c_char, *const c_char) {
        let next = (self.idx + 1) % 2;
        self.text[next] = CString::new(text).unwrap();
        self.sub_text[next] = CString::new(sub_text).unwrap();
        self.idx = next;

        (
            self.text[next].as_ptr() as *const c_char,
            self.sub_text[next].as_ptr() as *const c_char,
        )
    }

    /// Draws the spinner and returns immediately.
    ///
    /// Any previously drawn instance is released first.
    pub fn draw(&mut self, text: &str, sub_text: &str) -> Result<(), NbglPageError> {
        self.release();

        let (text, sub_text) = self.stage(text, sub_text);

        // `withLeftBorder` matches what `nbgl_pageDrawSpinner` and
        // `nbgl_useCaseSpinner` request for their layouts.
        let description = nbgl_layoutDescription_t {
            withLeftBorder: true,
            ..Default::default()
        };

        // SAFETY: `description` is only read for the duration of the call, and
        // both strings are owned by `self`, which outlives the drawn spinner
        // because `Drop` releases it.
        let handle = unsafe {
            let handle = nbgl_layoutGet(&description as *const nbgl_layoutDescription_t);
            if handle.is_null() {
                return Err(NbglPageError::DrawFailed);
            }

            nbgl_layoutAddSpinner(handle, text, sub_text, self.position);
            nbgl_layoutDraw(handle);
            nbgl_refreshSpecial(FULL_COLOR_PARTIAL_REFRESH);

            handle
        };

        self.handle = handle;
        self.generation = PAGE_GENERATION
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);

        Ok(())
    }

    /// Updates the displayed text without redrawing the whole page.
    ///
    /// Also advances the spinner, as `nbgl_useCaseSpinner` does on each call.
    /// `nbgl_layoutUpdateSpinner` takes an absolute position and moves the
    /// spinner to it, so passing an unchanged value would pin the animation
    /// and undo NBGL's ticker.
    ///
    /// Does nothing if the spinner is not currently live — redraw with
    /// [`draw`](NbglSpinnerPage::draw) in that case.
    pub fn update(&mut self, text: &str, sub_text: &str) {
        if !self.is_live() {
            return;
        }

        self.position = (self.position + 1) % NB_SPINNER_POSITIONS as u8;

        let (text, sub_text) = self.stage(text, sub_text);
        self.refresh(text, sub_text);
    }

    /// Advances the spinner by one position.
    ///
    /// Not normally needed — NBGL turns the spinner on its own ticker. Use
    /// this only to drive the animation manually.
    ///
    /// Does nothing if the spinner is not currently live.
    pub fn tick(&mut self) {
        if !self.is_live() {
            return;
        }

        self.position = (self.position + 1) % NB_SPINNER_POSITIONS as u8;

        let (text, sub_text) = (
            self.text[self.idx].as_ptr() as *const c_char,
            self.sub_text[self.idx].as_ptr() as *const c_char,
        );
        self.refresh(text, sub_text);
    }

    /// Pushes the current text and position into the live layout, refreshing
    /// the screen as `nbgl_useCaseSpinner` does: a fast black and white refresh
    /// when only the spinner moved, a partial colour refresh when text changed.
    fn refresh(&mut self, text: *const c_char, sub_text: *const c_char) {
        // SAFETY: `handle` is live (checked by the callers), and both pointers
        // target allocations owned by `self`.
        unsafe {
            match nbgl_layoutUpdateSpinner(self.handle, text, sub_text, self.position) {
                1 => nbgl_refreshSpecial(BLACK_AND_WHITE_FAST_REFRESH),
                2 => nbgl_refreshSpecial(FULL_COLOR_PARTIAL_REFRESH),
                _ => (),
            }
        }
    }

    /// Whether this spinner still owns the screen.
    ///
    /// Becomes `false` once another page or a blocking widget has drawn over
    /// it, at which point [`draw`](NbglSpinnerPage::draw) restores it.
    pub fn is_live(&self) -> bool {
        !self.handle.is_null() && PAGE_GENERATION.load(Ordering::Acquire) == self.generation
    }

    /// Releases the spinner. Idempotent, and called automatically by [`Drop`].
    pub fn release(&mut self) {
        if self.handle.is_null() {
            return;
        }

        // Only release while we still own the single background layout;
        // otherwise the handle now refers to someone else's layout.
        if PAGE_GENERATION.load(Ordering::Acquire) == self.generation {
            // SAFETY: `handle` came from a successful `nbgl_layoutGet`, has not
            // been released, and the generation check confirms it is still ours.
            unsafe { nbgl_layoutRelease(self.handle) };
            // No redraw: the caller decides what replaces this page.
        }

        self.handle = core::ptr::null_mut();
    }
}

impl Drop for NbglSpinnerPage {
    fn drop(&mut self) {
        self.release();
    }
}
