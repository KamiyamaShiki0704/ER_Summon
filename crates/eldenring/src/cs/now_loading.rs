/// Used by the game to determine what background image to display during a loading screen.
use crate::cs::{
    CSRandXorshift,
    task::{CSEzTask, CSEzUpdateTask},
};
use shared::OwnedPtr;

#[repr(C)]
/// Source of name: RTTI
///
/// Responsible for managing the loading screens timing and contents.
#[shared::singleton("CSNowLoadingHelper")]
pub struct CSNowLoadingHelper {
    vftable: u64,
    pub rand_xorshift: OwnedPtr<CSRandXorshift>,
    pub update_task: CSEzUpdateTask<CSEzTask, Self>,
    unk38: u64,
    unk40: u64,
    /// A list of all MENU_LOAD entries. Manipulating this list allows you to change what
    /// background images are used.
    pub menu_load_entries: [i32; 34],
    /// Incremented everytime the user enters the loading screen. Used as as index into
    /// `menu_load_entries` like: menu_load_entries[(menu_load_counter + 1) % 34].
    pub menu_load_counter: i32,
    unkd4: u32,
    scaleform_replace_text_info: u64,
    unke0: u64,
    unke8: f32,
    /// Unknown loading-screen state flags. At least one of these is expected to be active while
    /// the loading screen is still visible.
    pub loading_flags: [bool; 4],
    unkf0: u32,
    unkf4: u32,
}

impl CSNowLoadingHelper {
    pub fn is_loading_screen_active(&self) -> bool {
        self.loading_flags.iter().any(|flag| *flag)
    }
}

#[cfg(test)]
mod test {
    use std::mem::size_of;

    use super::CSNowLoadingHelper;

    #[test]
    fn proper_sizes() {
        assert_eq!(0xf8, size_of::<CSNowLoadingHelper>());
    }
}
