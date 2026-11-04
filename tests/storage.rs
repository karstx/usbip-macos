mod support;

use support::classes::{fat16, STORAGE_BLOCK};

fn u16_at(b: &[u8], i: usize) -> usize {
    u16::from_le_bytes([b[i], b[i + 1]]) as usize
}

fn u32_at(b: &[u8], i: usize) -> usize {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]) as usize
}

/// Reads the volume back the way a FAT driver would and checks it is a valid, empty FAT16.
fn check(bytes: usize) {
    let disk = fat16(bytes, "usbip test");
    assert_eq!(disk.len(), bytes);
    assert_eq!(&disk[510..512], &[0x55, 0xaa]);
    assert_eq!(u16_at(&disk, 11), STORAGE_BLOCK);
    let per_cluster = disk[13] as usize;
    let reserved = u16_at(&disk, 14);
    let fats = disk[16] as usize;
    let root_entries = u16_at(&disk, 17);
    let sectors = match u16_at(&disk, 19) {
        0 => u32_at(&disk, 32),
        n => n,
    };
    let fat_sectors = u16_at(&disk, 22);
    assert_eq!(sectors * STORAGE_BLOCK, bytes);

    let root_sectors = root_entries * 32 / STORAGE_BLOCK;
    let data = sectors - reserved - fats * fat_sectors - root_sectors;
    let clusters = data / per_cluster;
    assert!((4085..65525).contains(&clusters), "{clusters} clusters");
    assert!((clusters + 2) * 2 <= fat_sectors * STORAGE_BLOCK);

    for fat in 0..fats {
        let start = (reserved + fat * fat_sectors) * STORAGE_BLOCK;
        assert_eq!(&disk[start..start + 4], &[0xf8, 0xff, 0xff, 0xff]);
    }
    let root = (reserved + fats * fat_sectors) * STORAGE_BLOCK;
    assert_eq!(&disk[root..root + 11], b"USBIP TEST ");
    assert_eq!(disk[root + 11], 0x08);
    assert_eq!(&disk[43..54], b"USBIP TEST ");
}

#[test]
fn fat16_volumes() {
    check(4 << 20);
    check(64 << 20);
    check(512 << 20);
}
