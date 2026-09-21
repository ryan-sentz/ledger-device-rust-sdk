//! A wrapper around the asynchronous NBGL [nbgl_useCaseHomeAndSettings](https://github.com/LedgerHQ/ledger-secure-sdk/blob/master/lib_nbgl/src/nbgl_use_case.c#L3454) C API binding.
//!
//! Draws the extended version of home page of an app (page on which we land when launching it
//! from dashboard) with automatic support of setting display.
//! It enables to use an action button (see [NbglHomeAndSettings::action]), replacing the
//! deprecated `nbgl_useCaseHomeExt` C API.
use super::*;
use crate::io::{Reply, StatusWords};
use crate::io_callbacks::{nbgl_fetch_apdu_header, nbgl_reply_status};
use core::sync::atomic::{AtomicPtr, Ordering};

pub const SETTINGS_SIZE: usize = 10;
static NVM_REF: AtomicPtr<AtomicStorage<[u8; SETTINGS_SIZE]>> =
    AtomicPtr::new(core::ptr::null_mut());
static mut SWITCH_ARRAY: [nbgl_contentSwitch_t; SETTINGS_SIZE] =
    [unsafe { const_zero!(nbgl_contentSwitch_t) }; SETTINGS_SIZE];

/// User callback invoked when the home screen action button is pressed.
static mut HOME_ACTION_CB: Option<fn()> = None;

/// Callback triggered by the NBGL API when the home screen action button is pressed.
unsafe extern "C" fn home_action_cb() {
    unsafe {
        if let Some(cb) = HOME_ACTION_CB {
            cb();
        }
    }
}

/// Style of the home screen action button.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HomeActionStyle {
    /// Black button, implicating the main action of the app.
    #[default]
    Strong,
    /// White button, more for extended features.
    Soft,
}

impl From<HomeActionStyle> for nbgl_homeActionStyle_t {
    fn from(style: HomeActionStyle) -> Self {
        match style {
            HomeActionStyle::Strong => STRONG_HOME_ACTION,
            HomeActionStyle::Soft => SOFT_HOME_ACTION,
        }
    }
}

/// Callback triggered by the NBGL API when a setting switch is toggled.
unsafe extern "C" fn settings_callback(token: c_int, _index: u8, _page: c_int) {
    unsafe {
        let idx = token - FIRST_USER_TOKEN as i32;
        if idx < 0 || idx >= SETTINGS_SIZE as i32 {
            panic!("Invalid token.");
        }

        let setting_idx: usize = idx as usize;

        match SWITCH_ARRAY[setting_idx].initState {
            OFF_STATE => SWITCH_ARRAY[setting_idx].initState = ON_STATE,
            ON_STATE => SWITCH_ARRAY[setting_idx].initState = OFF_STATE,
            _ => panic!("Invalid state."),
        }
        let ptr = NVM_REF.load(Ordering::Relaxed);
        if !ptr.is_null() {
            let data = &mut *ptr;
            let mut switch_values: [u8; SETTINGS_SIZE] = *data.get_ref();
            if switch_values[setting_idx] == OFF_STATE {
                switch_values[setting_idx] = ON_STATE;
            } else {
                switch_values[setting_idx] = OFF_STATE;
            }
            data.update(&switch_values);
        }
    }
}

/// Informations fields name to display in the dedicated
/// page of the home screen.
const INFO_FIELDS: [*const c_char; 2] = [c"Version".as_ptr(), c"Developer".as_ptr()];

/// Initial page to display when showing the home and settings screen.
pub enum PageIndex {
    Settings(u8),
    Home,
}

/// A builder to create and show a home and settings page.
pub struct NbglHomeAndSettings {
    app_name: CString,
    tag_line: Option<CString>,
    info_contents: Vec<CString>,
    info_contents_ptr: Vec<*const c_char>,
    setting_contents: Vec<[CString; 2]>,
    nb_settings: u8,
    content: nbgl_content_t,
    generic_contents: nbgl_genericContents_t,
    info_list: nbgl_contentInfoList_t,
    icon: nbgl_icon_details_t,
    start_page: PageIndex,
    action_text: Option<CString>,
    action_icon: Option<nbgl_icon_details_t>,
    action_style: HomeActionStyle,
    action: nbgl_homeAction_t,
}

impl SyncNBGL for NbglHomeAndSettings {}

unsafe extern "C" fn quit_cb() {
    exit_app(0);
}

impl Default for NbglHomeAndSettings {
    fn default() -> Self {
        Self::new()
    }
}

impl NbglHomeAndSettings {
    /// Creates a new home and settings page builder.
    /// # Returns
    /// Returns a new instance of `NbglHomeAndSettings`.
    pub fn new() -> NbglHomeAndSettings {
        NbglHomeAndSettings {
            app_name: CString::new("").unwrap(),
            tag_line: None,
            info_contents: Vec::default(),
            info_contents_ptr: Vec::default(),
            setting_contents: Vec::default(),
            nb_settings: 0,
            content: nbgl_content_t::default(),
            generic_contents: nbgl_genericContents_t::default(),
            info_list: nbgl_contentInfoList_t::default(),
            icon: nbgl_icon_details_t::default(),
            start_page: PageIndex::Home,
            action_text: None,
            action_icon: None,
            action_style: HomeActionStyle::default(),
            action: nbgl_homeAction_t::default(),
        }
    }

    /// Sets the icon to display in the center of the page.
    /// # Arguments
    /// * `glyph` - The icon to display in the center of the page.
    /// # Returns
    /// Returns the builder itself to allow method chaining.
    pub fn glyph(self, glyph: &NbglGlyph) -> NbglHomeAndSettings {
        let icon = glyph.into();
        NbglHomeAndSettings { icon, ..self }
    }

    /// Sets the application informations to display in the dedicated
    /// page of the home screen.
    /// # Arguments
    /// * `app_name` - The name of the application.
    /// * `version` - The version of the application.
    /// * `author` - The author of the application.
    /// # Returns
    /// Returns the builder itself to allow method chaining.
    pub fn infos(self, app_name: &str, version: &str, author: &str) -> NbglHomeAndSettings {
        let v: Vec<CString> = vec![
            CString::new(version).unwrap(),
            CString::new(author).unwrap(),
        ];

        NbglHomeAndSettings {
            app_name: CString::new(app_name).unwrap(),
            info_contents: v,
            ..self
        }
    }

    /// Sets the tagline to display below the application name on the home screen.
    /// # Arguments
    /// * `tagline` - The tagline to display below the application name on the home screen.
    /// # Returns
    /// Returns the builder itself to allow method chaining.
    pub fn tagline(self, tagline: &str) -> NbglHomeAndSettings {
        NbglHomeAndSettings {
            tag_line: Some(CString::new(tagline).unwrap()),
            ..self
        }
    }

    /// Adds an action button to the home screen, displayed above the "Quit app" button.
    ///
    /// The button uses [HomeActionStyle::Strong] unless changed with
    /// [NbglHomeAndSettings::action_style].
    ///
    /// `callback` is invoked from within NBGL event dispatch, so it must not call blocking NBGL
    /// APIs (e.g. `show()`). Either set a flag (e.g. an `AtomicBool`) and react to it from the
    /// application loop, or display another page with a non-blocking API. Note that with
    /// `io_new`, `Comm::next_command` blocks until an APDU arrives, so a flag is only observed
    /// once the next command is received.
    ///
    /// Only one home screen action callback is registered at a time; the most recently
    /// configured one is used.
    /// # Arguments
    /// * `text` - The text of the action button.
    /// * `callback` - The function to call when the action button is pressed.
    /// # Returns
    /// Returns the builder itself to allow method chaining.
    pub fn action(self, text: &str, callback: fn()) -> NbglHomeAndSettings {
        unsafe {
            HOME_ACTION_CB = Some(callback);
        }
        NbglHomeAndSettings {
            action_text: Some(CString::new(text).unwrap()),
            ..self
        }
    }

    /// Sets the icon to display in the home screen action button.
    /// # Arguments
    /// * `glyph` - The icon to display in the action button.
    /// # Returns
    /// Returns the builder itself to allow method chaining.
    pub fn action_glyph(self, glyph: &NbglGlyph) -> NbglHomeAndSettings {
        NbglHomeAndSettings {
            action_icon: Some(glyph.into()),
            ..self
        }
    }

    /// Sets the style of the home screen action button.
    /// # Arguments
    /// * `style` - The style of the action button.
    /// # Returns
    /// Returns the builder itself to allow method chaining.
    pub fn action_style(self, style: HomeActionStyle) -> NbglHomeAndSettings {
        NbglHomeAndSettings {
            action_style: style,
            ..self
        }
    }

    /// Sets the settings to display in the settings page.
    /// # Arguments
    /// * `nvm_data` - A mutable reference to an `AtomicStorage` containing the settings data.
    /// * `settings_strings` - A slice of tuples containing the setting name and description.
    /// # Panics
    /// Panics if the number of settings exceeds [SETTINGS_SIZE].
    /// # Returns
    /// Returns the builder itself to allow method chaining.
    pub fn settings(
        self,
        nvm_data: &mut AtomicStorage<[u8; SETTINGS_SIZE]>,
        settings_strings: &[[&str; 2]],
    ) -> NbglHomeAndSettings {
        NVM_REF.store(
            nvm_data as *mut AtomicStorage<[u8; SETTINGS_SIZE]>,
            Ordering::Relaxed,
        );

        if settings_strings.len() > SETTINGS_SIZE {
            panic!("Too many settings.");
        }

        let v: Vec<[CString; 2]> = settings_strings
            .iter()
            .map(|s| [CString::new(s[0]).unwrap(), CString::new(s[1]).unwrap()])
            .collect();

        NbglHomeAndSettings {
            nb_settings: settings_strings.len() as u8,
            setting_contents: v,
            ..self
        }
    }

    /// Sets the initial page to display when showing the home and settings screen.
    /// # Arguments
    /// * `page` - The initial page to display.
    /// # Returns
    /// Returns the builder itself to allow method chaining.
    pub fn set_start_page(&mut self, page: PageIndex) {
        self.start_page = page;
    }

    /// Build the C structures referenced by `nbgl_useCaseHomeAndSettings`.
    ///
    /// These are stored in `self` because the C SDK keeps pointers to them while the home
    /// screen is displayed, so `self` must not be moved after showing the screen.
    fn prepare(&mut self) {
        unsafe {
            self.info_contents_ptr = self
                .info_contents
                .iter()
                .map(|s| s.as_ptr())
                .collect::<Vec<_>>();

            self.info_list = nbgl_contentInfoList_t {
                infoTypes: INFO_FIELDS.as_ptr(),
                infoContents: self.info_contents_ptr[..].as_ptr(),
                nbInfos: INFO_FIELDS.len() as u8,
                infoExtensions: core::ptr::null(),
                token: 0,
                withExtensions: false,
            };

            for (i, setting) in self.setting_contents.iter().enumerate() {
                SWITCH_ARRAY[i].text = setting[0].as_ptr();
                SWITCH_ARRAY[i].subText = setting[1].as_ptr();
                let ptr = NVM_REF.load(Ordering::Relaxed);
                let state = if !ptr.is_null() {
                    (&*ptr).get_ref()[i]
                } else {
                    OFF_STATE
                };
                SWITCH_ARRAY[i].initState = state;
                SWITCH_ARRAY[i].token = (FIRST_USER_TOKEN + i as u32) as u8;
                #[cfg(any(target_os = "stax", target_os = "flex", target_os = "apex_p"))]
                {
                    SWITCH_ARRAY[i].tuneId = TuneIndex::TapCasual as u8;
                }
            }

            self.content = nbgl_content_t {
                content: nbgl_content_u {
                    switchesList: nbgl_pageSwitchesList_s {
                        switches: &raw const SWITCH_ARRAY as *const nbgl_contentSwitch_t,
                        nbSwitches: self.nb_settings,
                    },
                },
                contentActionCallback: Some(settings_callback),
                type_: SWITCHES_LIST,
            };

            self.generic_contents = nbgl_genericContents_t {
                callbackCallNeeded: false,
                __bindgen_anon_1: nbgl_genericContents_t__bindgen_ty_1 {
                    contentsList: &self.content as *const nbgl_content_t,
                },
                nbContents: 1,
            };

            self.action = match (&self.action_text, &self.action_icon) {
                (None, None) => nbgl_homeAction_t::default(),
                (text, icon) => nbgl_homeAction_t {
                    text: match text {
                        Some(t) => t.as_ptr(),
                        None => core::ptr::null(),
                    },
                    icon: match icon {
                        Some(i) => i as *const nbgl_icon_details_t,
                        None => core::ptr::null(),
                    },
                    callback: Some(home_action_cb),
                    style: self.action_style.into(),
                },
            };
        }
    }

    /// Returns a pointer to the action button description, or null if no action is set.
    fn action_ptr(&self) -> *const nbgl_homeAction_t {
        if self.action_text.is_some() || self.action_icon.is_some() {
            &self.action as *const nbgl_homeAction_t
        } else {
            core::ptr::null()
        }
    }

    /// Show the home screen and settings page (internal implementation).
    fn show_internal<T: TryFrom<ApduHeader>>(&mut self) -> Event<T>
    where
        Reply: From<<T as TryFrom<ApduHeader>>::Error>,
    {
        unsafe {
            loop {
                self.prepare();

                self.ux_sync_init();
                nbgl_useCaseHomeAndSettings(
                    self.app_name.as_ptr() as *const c_char,
                    &self.icon as *const nbgl_icon_details_t,
                    match self.tag_line {
                        None => core::ptr::null(),
                        Some(ref tag) => tag.as_ptr() as *const c_char,
                    },
                    match self.start_page {
                        PageIndex::Home => INIT_HOME_PAGE as u8,
                        PageIndex::Settings(idx) => idx,
                    },
                    match self.nb_settings {
                        0 => core::ptr::null(),
                        _ => &self.generic_contents as *const nbgl_genericContents_t,
                    },
                    &self.info_list as *const nbgl_contentInfoList_t,
                    self.action_ptr(),
                    Some(quit_callback),
                );
                match self.ux_sync_wait(true) {
                    SyncNbgl::UxSyncRetApduReceived => {
                        if let Some(hdr) = nbgl_fetch_apdu_header() {
                            // Reconstruct minimal Event::Command using APDU header only.
                            // The generic parameter T: TryFrom<ApduHeader> will parse header.
                            match T::try_from(hdr) {
                                Ok(ins) => {
                                    return Event::Command(ins);
                                }
                                _ => {
                                    // In case of parse error we emulate a BadIns reply.
                                    nbgl_reply_status(Reply(StatusWords::BadIns as u16));
                                }
                            }
                        }
                    }
                    SyncNbgl::UxSyncRetQuitted => {
                        exit_app(0);
                    }
                    _ => {
                        panic!("Unexpected return value from ux_sync_homeAndSettings");
                    }
                }
            }
        }
    }

    /// Show the home screen and settings page.
    /// This function will block until an APDU is received or the user quits the app.
    /// # Arguments
    /// * `_comm` - Mutable reference to Comm.
    #[cfg(feature = "io_new")]
    #[deprecated(
        since = "1.37.0",
        note = "blocking on an APDU forces the home screen to be refreshed for every received APDU; use `show_and_return` instead"
    )]
    pub fn show<T: TryFrom<ApduHeader>, const N: usize>(
        &mut self,
        _comm: &mut crate::io::Comm<N>,
    ) -> Event<T>
    where
        Reply: From<<T as TryFrom<ApduHeader>>::Error>,
    {
        self.show_internal()
    }

    /// Show the home screen and settings page.
    /// This function will block until an APDU is received or the user quits the app.
    #[cfg(not(feature = "io_new"))]
    #[deprecated(
        since = "1.37.0",
        note = "blocking on an APDU forces the home screen to be refreshed for every received APDU; use `show_and_return` instead"
    )]
    pub fn show<T: TryFrom<ApduHeader>>(&mut self) -> Event<T>
    where
        Reply: From<<T as TryFrom<ApduHeader>>::Error>,
    {
        self.show_internal()
    }

    /// Show the home screen and settings page.
    /// This function returns immediately after the screen is displayed.
    pub fn show_and_return(&mut self) {
        // Takes over the single non-modal NBGL layout, displacing any live
        // `NbglPage`. Unlike the blocking widgets this does not go through
        // `SyncNBGL::ux_sync_init`, so the counter is bumped explicitly.
        #[cfg(any(target_os = "stax", target_os = "flex", target_os = "apex_p"))]
        super::nbgl_page::PAGE_GENERATION.fetch_add(1, core::sync::atomic::Ordering::AcqRel);

        self.prepare();

        unsafe {
            nbgl_useCaseHomeAndSettings(
                self.app_name.as_ptr() as *const c_char,
                &self.icon as *const nbgl_icon_details_t,
                match self.tag_line {
                    None => core::ptr::null(),
                    Some(ref tag) => tag.as_ptr() as *const c_char,
                },
                match self.start_page {
                    PageIndex::Home => INIT_HOME_PAGE as u8,
                    PageIndex::Settings(idx) => idx,
                },
                match self.nb_settings {
                    0 => core::ptr::null(),
                    _ => &self.generic_contents as *const nbgl_genericContents_t,
                },
                &self.info_list as *const nbgl_contentInfoList_t,
                self.action_ptr(),
                Some(quit_cb),
            );
        }
    }
}
