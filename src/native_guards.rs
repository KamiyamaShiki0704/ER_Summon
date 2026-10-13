//! Regressions for the SDK's exact executable/loaded-span gate, using owned native fixtures.
//! Original fuzzy consumer guards are superseded by full spans and file identity.
use sha2::{Digest, Sha256};
use shikifsrs_runtime::{
    memory::SliceMemory,
    profile::{CodeRange, ExecutableIdentity, VerifiedFile},
};
fn gate(code: &[u8]) {
    let mut bytes = vec![0u8; 0xa00];
    bytes[..2].copy_from_slice(b"MZ");
    for (at, value) in [
        (0x3c, 0x80u32),
        (0xd0, 0x2000),
        (0xd4, 0x200),
        (0x190, 0x800),
        (0x194, 0x1000),
        (0x198, 0x800),
        (0x19c, 0x200),
        (0x1ac, 0x60000020),
    ] {
        bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }
    bytes[0x80..0x84].copy_from_slice(b"PE\0\0");
    for (at, value) in [(0x84, 0x8664u16), (0x86, 1), (0x94, 0xf0), (0x98, 0x20b)] {
        bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
    }
    bytes[0xb0..0xb8].copy_from_slice(&0x140000000u64.to_le_bytes());
    bytes[0x188..0x18d].copy_from_slice(b".text");
    bytes[0x200..0x200 + code.len()].copy_from_slice(code);
    let identity = ExecutableIdentity {
        name: "owned synthetic guard fixture",
        file_size: bytes.len(),
        sha256: Sha256::digest(&bytes).into(),
    };
    let file = VerifiedFile::verify(&bytes, identity).unwrap();
    let range = CodeRange {
        rva: 0x1000,
        length: code.len(),
    };
    let mut mapped = vec![0u8; 0x2000];
    mapped[..0x200].copy_from_slice(&bytes[..0x200]);
    mapped[0x1000..0x1000 + code.len()].copy_from_slice(code);
    let memory = SliceMemory::new(0x140000000, &mapped).unwrap();
    file.bind(&memory, 0x140000000, &[range]).unwrap();
    for offset in [0, code.len() / 2, code.len() - 1] {
        let mut changed = mapped.clone();
        changed[0x1000 + offset] ^= 1;
        let memory = SliceMemory::new(0x140000000, &changed).unwrap();
        assert!(file.bind(&memory, 0x140000000, &[range]).is_err());
    }
    let truncated = SliceMemory::new(0x140000000, &mapped[..0x1000 + code.len() - 1]).unwrap();
    assert!(file.bind(&truncated, 0x140000000, &[range]).is_err());
    let mut duplicate = bytes.clone();
    duplicate.extend_from_slice(code);
    assert!(VerifiedFile::verify(&duplicate, identity).is_err());
    let mut altered = bytes.clone();
    altered[0x200 + code.len() / 2] ^= 1;
    assert!(VerifiedFile::verify(&altered, identity).is_err());
}
pub fn assert_creation_guard() {
    assert_eq!(
        std::mem::align_of::<shikifsrs_elden_ring::current_character_spatial::NativeEulerBits>(),
        16
    );
    assert!(shikifsrs_elden_ring::Ww171File::verify(&[]).is_err());
}
pub fn assert_retirement_guards() {
    gate(include_bytes!(
        "../tests/fixtures/ww271-chrset-free-prefix.bin"
    ));
    gate(include_bytes!("../tests/fixtures/ww271-task-free.bin"));
}
#[test]
fn event_groups_binding_rejects_modified_or_truncated_native_body() {
    gate(include_bytes!(
        "../tests/fixtures/ww271-register-event-groups.bin"
    ));
}
#[test]
fn owner_lookup_rejects_modified_duplicate_or_truncated_native_body() {
    gate(include_bytes!(
        "../tests/fixtures/ww271-chrset-free-prefix.bin"
    ));
}
#[test]
fn native_detach_and_task_free_are_verified_before_mutation() {
    gate(include_bytes!("../tests/fixtures/ww271-task-free.bin"));
}
