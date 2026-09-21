use super::*;

pub use super::nbgl_content::{
    CenteredInfo, CenteredInfoStyle, InfoButton, InfoLongPress, InfosList, NbglPageContent,
    TagValueConfirm, TagValueList,
};

/// Builder for a multi-page generic review screen backed by the NBGL
/// `nbgl_useCaseGenericReview` C API.
///
/// Use this when you need full control over the pages shown during a
/// review flow. Content elements are added one by one with
/// [`add_content`](NbglGenericReview::add_content) and then presented
/// to the user via [`show`](NbglGenericReview::show).
///
/// # Example
///
/// ```rust,ignore
/// let approved = NbglGenericReview::new()
///     .add_content(NbglPageContent::TagValueConfirm(
///         TagValueConfirm::new(&fields, TuneIndex::TapCasual, "Approve", "Reject"),
///     ))
///     .show("Reject transaction");
/// ```
pub struct NbglGenericReview {
    content_list: Vec<NbglPageContent>,
}

impl SyncNBGL for NbglGenericReview {}

impl Default for NbglGenericReview {
    fn default() -> Self {
        Self::new()
    }
}

impl NbglGenericReview {
    /// Creates an empty [`NbglGenericReview`] with no content pages.
    pub fn new() -> NbglGenericReview {
        NbglGenericReview {
            content_list: Vec::new(),
        }
    }

    /// Appends a content page to the review.
    ///
    /// This method consumes and returns `self` so that calls can be chained:
    ///
    /// ```rust,ignore
    /// let review = NbglGenericReview::new()
    ///     .add_content(NbglPageContent::CenteredInfo(info))
    ///     .add_content(NbglPageContent::TagValueList(fields));
    /// ```
    pub fn add_content(mut self, content: NbglPageContent) -> NbglGenericReview {
        self.content_list.push(content);
        self
    }

    /// Converts the Rust content list into the C representation expected by
    /// the NBGL library.
    fn to_c_content_list(&self) -> Vec<nbgl_content_t> {
        self.content_list
            .iter()
            .map(|content| content.into())
            .collect()
    }

    fn show_internal(&self, reject_button_str: &str) -> bool {
        unsafe {
            let c_content_list: Vec<nbgl_content_t> = self.to_c_content_list();

            let content_struct = nbgl_genericContents_t {
                callbackCallNeeded: false,
                __bindgen_anon_1: nbgl_genericContents_t__bindgen_ty_1 {
                    contentsList: c_content_list.as_ptr(),
                },
                nbContents: self.content_list.len() as u8,
            };

            let reject_button_cstring = CString::new(reject_button_str).unwrap();

            self.ux_sync_init();
            nbgl_useCaseGenericReview(
                &content_struct as *const nbgl_genericContents_t,
                reject_button_cstring.as_ptr() as *const c_char,
                Some(rejected_callback),
            );
            let sync_ret = self.ux_sync_wait(false);

            // Return true if the user approved the transaction, false otherwise.
            matches!(sync_ret, SyncNbgl::UxSyncRetApproved)
        }
    }

    /// Displays the review to the user and blocks until a decision is made.
    ///
    /// A reject button labelled with `reject_button_str` is shown on the
    /// final page. The method returns `true` if the user approved the review
    /// and `false` if they rejected it.
    ///
    /// # Arguments
    ///
    /// * `_comm` - Mutable reference to Comm.
    /// * `reject_button_str` — Text for the reject/cancel button displayed
    ///   at the end of the review flow (e.g. `"Reject transaction"`).
    #[cfg(feature = "io_new")]
    pub fn show<const N: usize>(
        &self,
        _comm: &mut crate::io::Comm<N>,
        reject_button_str: &str,
    ) -> bool {
        self.show_internal(reject_button_str)
    }

    /// Displays the review to the user and blocks until a decision is made.
    ///
    /// A reject button labelled with `reject_button_str` is shown on the
    /// final page. The method returns `true` if the user approved the review
    /// and `false` if they rejected it.
    ///
    /// # Arguments
    ///
    /// * `reject_button_str` — Text for the reject/cancel button displayed
    ///   at the end of the review flow (e.g. `"Reject transaction"`).
    #[cfg(not(feature = "io_new"))]
    pub fn show(&self, reject_button_str: &str) -> bool {
        self.show_internal(reject_button_str)
    }
}
