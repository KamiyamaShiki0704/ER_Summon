use pelite::pe64::PeView;
use std::sync::LazyLock;
use windows::Win32::System::LibraryLoader::GetModuleHandleA;
use windows::core::PCSTR;

mod bundle;
mod rva_jp;
mod rva_ww;
mod rva_ww_270;

pub use bundle::RvaBundle;

use fromsoftware_shared::game_version::{DetectError, GameVersion, LANG_ID_EN, LANG_ID_JP};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ERGameVersion {
    Ww262,
    Ww270,
    Jp2621,
}

impl GameVersion for ERGameVersion {
    const NAME: &'static str = "elden ring";

    fn from_lang_version(lang_id: u16, version: &str) -> Option<Self> {
        match (lang_id, version) {
            (LANG_ID_EN, "2.6.2.0") => Some(Self::Ww262),
            (LANG_ID_EN, "2.7.0.0") => Some(Self::Ww270),
            (LANG_ID_JP, "2.6.2.1") => Some(Self::Jp2621),
            _ => None,
        }
    }
}

impl ERGameVersion {
    const fn rvas(self) -> RvaBundle {
        match self {
            Self::Ww262 => rva_ww::RVAS,
            Self::Ww270 => rva_ww_270::RVAS,
            Self::Jp2621 => rva_jp::RVAS,
        }
    }

    const fn uses_chr_ins_117_layout(self) -> bool {
        matches!(self, Self::Ww270)
    }
}

struct DetectedRvas {
    version: ERGameVersion,
    rvas: RvaBundle,
}

fn detected() -> &'static Result<DetectedRvas, DetectError> {
    static RVAS: LazyLock<Result<DetectedRvas, DetectError>> = LazyLock::new(|| {
        let module = unsafe {
            PeView::module(GetModuleHandleA(PCSTR(std::ptr::null())).unwrap().0 as *const u8)
        };
        ERGameVersion::detect(&module).map(|version| DetectedRvas {
            version,
            rvas: version.rvas(),
        })
    });

    &RVAS
}

pub(crate) fn try_get() -> Result<&'static RvaBundle, &'static DetectError> {
    match detected() {
        Ok(detected) => Ok(&detected.rvas),
        Err(error) => Err(error),
    }
}

pub(crate) fn uses_chr_ins_117_layout() -> bool {
    matches!(
        detected(),
        Ok(detected) if detected.version.uses_chr_ins_117_layout()
    )
}

/// Returns the RVA bundle for the current executable region and version.
///
/// This will panic if the current executable isn't supported by this package.
pub fn get() -> &'static RvaBundle {
    try_get().unwrap_or_else(|error| panic!("{error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_ww_270_profile() {
        let version = ERGameVersion::from_lang_version(LANG_ID_EN, "2.7.0.0")
            .expect("WW 2.7.0.0 should be recognized");
        let rvas = version.rvas();

        assert_eq!(rvas.global_hinstance, 0x3d89708);
        assert_eq!(rvas.register_task, 0xeb3de0);
        assert_eq!(rvas.chr_ins_apply_speffect, 0x3e8dc0);
        assert_eq!(rvas.chr_ins_remove_speffect, 0x3ee2e0);
        assert!(version.uses_chr_ins_117_layout());
        assert!(!ERGameVersion::Ww262.uses_chr_ins_117_layout());
        assert!(!ERGameVersion::Jp2621.uses_chr_ins_117_layout());
    }

    #[test]
    fn rejects_unknown_ww_version() {
        assert_eq!(
            ERGameVersion::from_lang_version(LANG_ID_EN, "9.9.9.9"),
            None
        );
    }
}
