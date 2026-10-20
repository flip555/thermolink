use bytes::Bytes;
use thermolink::camera::uvc_radiometric::layout::ExtractedFrame;
use thermolink::camera::uvc_radiometric::types::{DeviceTemp, RadiometrySpec};

fn put_u16(buf: &mut [u8], index: usize, value: u16) {
    buf[index * 2..index * 2 + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_f32(buf: &mut [u8], index: usize, value: f32) {
    buf[index * 2..index * 2 + 4].copy_from_slice(&value.to_le_bytes());
}

#[test]
fn linear_profile_returns_its_calibration() {
    let frame = ExtractedFrame {
        width: 256,
        height: 192,
        raw: Bytes::new(),
        metadata: None,
    };
    let (cal, lut) = RadiometrySpec::Linear {
        scale: 0.015625,
        offset: -273.15,
    }
    .for_frame(&frame);
    assert!(lut.is_none());
    assert_eq!(cal.scale, 0.015625);
    assert_eq!(cal.offset, -273.15);
}

#[test]
fn tc002c_empirical_device_temp_offsets_calibration() {
    let device_temp = DeviceTemp {
        row: 0,
        offset: 2,
        divisor: 44.0,
        offset_c: -3.0,
        big_endian: false,
    };
    let mut metadata = vec![0u8; 256 * 2 * 4];
    let raw = ((25.0 + 3.0) * 44.0) as u16;
    metadata[2..4].copy_from_slice(&raw.to_le_bytes());

    let frame = ExtractedFrame {
        width: 256,
        height: 192,
        raw: Bytes::new(),
        metadata: Some(Bytes::from(metadata)),
    };
    let (cal, lut) = RadiometrySpec::EmpiricalDeviceTemp {
        scale: 0.03125,
        offset: -171.0,
        device_temp,
    }
    .for_frame(&frame);

    assert!(lut.is_none());
    assert!((cal.scale - 0.03125).abs() < 1e-6);
    assert!((cal.offset - (-171.0 + 25.0 * 1.1)).abs() < 1e-3);
}

#[test]
fn ht301_table_builds_a_finite_lut() {
    let width = 384usize;
    let height = 288usize;
    let amount_pixels = width * 3;
    let user_area = amount_pixels + 127;
    let mut metadata = vec![0u8; width * 4 * 2];

    put_u16(&mut metadata, 1, 7800); // fpa_tmp -> tfpa 20.0
    put_u16(&mut metadata, amount_pixels, 1000); // cal_00
    put_u16(&mut metadata, amount_pixels + 1, 2982); // shutter 25 C
    put_f32(&mut metadata, amount_pixels + 3, 1.0); // cal_01
    put_f32(&mut metadata, amount_pixels + 5, 100.0); // cal_02
    put_f32(&mut metadata, amount_pixels + 7, 0.0);
    put_f32(&mut metadata, amount_pixels + 9, 0.0);
    put_f32(&mut metadata, amount_pixels + 11, 0.0);
    put_f32(&mut metadata, user_area, 0.0); // correction
    put_f32(&mut metadata, user_area + 2, 20.0); // reflected
    put_f32(&mut metadata, user_area + 4, 20.0); // air
    put_f32(&mut metadata, user_area + 6, 50.0); // humidity
    put_f32(&mut metadata, user_area + 8, 0.95); // emissivity
    put_u16(&mut metadata, user_area + 10, 10); // distance

    let frame = ExtractedFrame {
        width: width as u32,
        height: height as u32,
        raw: Bytes::new(),
        metadata: Some(Bytes::from(metadata)),
    };
    let (cal, lut) = RadiometrySpec::TableFromFrame {
        metadata_row: 0,
        fpa_off: 7800.0,
        fpa_div: 36.0,
        cal00_offset: 390.0,
        cal00_fpamul: 7.05,
        range_c: 120,
    }
    .for_frame(&frame);

    assert_eq!(cal.scale, 1.0);
    let lut = lut.expect("table LUT");
    assert_eq!(lut.len(), 65_536);
    assert!(
        lut.iter().any(|value| value.is_finite()),
        "expected at least one finite temperature"
    );
}

#[test]
fn table_without_metadata_falls_back_cleanly() {
    let frame = ExtractedFrame {
        width: 256,
        height: 192,
        raw: Bytes::new(),
        metadata: None,
    };
    let (cal, lut) = RadiometrySpec::TableFromFrame {
        metadata_row: 0,
        fpa_off: 8617.0,
        fpa_div: 37.682,
        cal00_offset: 170.0,
        cal00_fpamul: 0.0,
        range_c: 120,
    }
    .for_frame(&frame);
    assert_eq!(cal.scale, 1.0);
    assert!(lut.is_none());
}
