use thermolink::camera::uvc_radiometric::FrameLayout;

#[test]
fn split_yuyv_extracts_the_bottom_half() {
    let width = 4u32;
    let height = 2u32;
    let frame_height = 4u32;
    let mut frame = vec![0u8; (width * frame_height * 2) as usize];
    // Top half is the visible YUYV image and must not leak into the raw block.
    frame[0] = 0xff;
    let start = (height * width * 2) as usize;
    frame[start..start + 2].copy_from_slice(&1234u16.to_le_bytes());
    frame[start + 2..start + 4].copy_from_slice(&5678u16.to_le_bytes());

    let layout = FrameLayout::SplitYuyvU16 {
        width,
        height,
        frame_height,
        thermal_half: thermolink::camera::uvc_radiometric::types::ThermalHalf::Bottom,
        endian: thermolink::camera::uvc_radiometric::types::Endian::Little,
    };
    let out = layout.extract(&frame).expect("extract");
    assert_eq!((out.width, out.height), (4, 2));
    assert_eq!(out.raw.len(), 4 * 2 * 2);
    assert_eq!(u16::from_le_bytes([out.raw[0], out.raw[1]]), 1234);
    assert_eq!(u16::from_le_bytes([out.raw[2], out.raw[3]]), 5678);
}

#[test]
fn packed_yuyv_reconstructs_raw16_pairs() {
    let width = 4u32;
    let layout = FrameLayout::PackedYuyvU16 {
        width,
        height: 1,
        frame_height: 2,
        thermal_rows: 1,
        metadata_rows: 1,
        image_start_row: 1,
        device_temp: None,
    };
    let mut frame = vec![0u8; 16];
    frame[..8].copy_from_slice(&[
        0x34, 0x12, 0x78, 0x56, // Y0,U0,Y1,V0 -> 0x1234, 0x5678
        0xbc, 0x9a, 0xf0, 0xde, // -> 0x9abc, 0xdef0
    ]);
    let out = layout.extract(&frame).expect("extract");
    assert_eq!((out.width, out.height), (4, 1));
    let vals: Vec<u16> = out
        .raw
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    assert_eq!(vals, vec![0x1234, 0x5678, 0x9abc, 0xdef0]);
}

#[test]
fn short_frames_are_errors_not_panics() {
    let layout = FrameLayout::SplitYuyvU16 {
        width: 256,
        height: 192,
        frame_height: 384,
        thermal_half: thermolink::camera::uvc_radiometric::types::ThermalHalf::Bottom,
        endian: thermolink::camera::uvc_radiometric::types::Endian::Little,
    };
    assert!(layout.extract(&[0u8; 10]).is_err());
}
