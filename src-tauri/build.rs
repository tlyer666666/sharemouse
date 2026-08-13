use std::path::PathBuf;

fn main() {
    let manifest =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR missing"));
    let generated = manifest.join("target").join("generated");
    std::fs::create_dir_all(&generated).expect("failed to create generated asset directory");
    let icon_path = generated.join("deskbridge.ico");
    let png_path = generated.join("deskbridge.png");
    std::fs::write(&icon_path, windows_icon()).expect("failed to create generated app icon");
    write_png_icon(&png_path);

    let attributes = if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let windows = tauri_build::WindowsAttributes::new()
            .window_icon_path(icon_path)
            .app_manifest(include_str!("windows-manifest.xml"));
        tauri_build::Attributes::new().windows_attributes(windows)
    } else {
        tauri_build::Attributes::new()
    };

    if let Err(error) = tauri_build::try_build(attributes) {
        eprintln!("DeskBridge build setup failed: {error:#}");
        std::process::exit(1);
    }
}

fn write_png_icon(path: &std::path::Path) {
    const SIDE: usize = 64;
    let mut pixels = vec![0_u8; SIDE * SIDE * 4];
    for y in 0..SIDE {
        for x in 0..SIDE {
            let back = (8..=42).contains(&x) && (8..=37).contains(&y);
            let back_border = back && (x <= 12 || x >= 38 || y <= 12 || y >= 33);
            let front = (23..=57).contains(&x) && (24..=53).contains(&y);
            let front_border = front && (x <= 27 || x >= 53 || y <= 28 || y >= 49);
            let stand = (37..=43).contains(&x) && (54..=61).contains(&y);
            let (red, green, blue, alpha) = if front_border || back_border || stand {
                (84, 229, 219, 255)
            } else if front {
                (18, 38, 48, 255)
            } else if back {
                (25, 31, 43, 255)
            } else {
                (0, 0, 0, 0)
            };
            let offset = (y * SIDE + x) * 4;
            pixels[offset..offset + 4].copy_from_slice(&[red, green, blue, alpha]);
        }
    }

    let file = std::fs::File::create(path).expect("failed to create generated PNG icon");
    let mut encoder = png::Encoder::new(file, SIDE as u32, SIDE as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().expect("failed to write PNG header");
    writer
        .write_image_data(&pixels)
        .expect("failed to write PNG icon");
}

fn windows_icon() -> Vec<u8> {
    const SIDE: usize = 16;
    const MASK_ROW_BYTES: usize = 4;
    let pixel_bytes = SIDE * SIDE * 4;
    let mask_bytes = MASK_ROW_BYTES * SIDE;
    let image_bytes = 40 + pixel_bytes + mask_bytes;
    let mut icon = Vec::with_capacity(22 + image_bytes);

    push_u16(&mut icon, 0);
    push_u16(&mut icon, 1);
    push_u16(&mut icon, 1);
    icon.extend_from_slice(&[SIDE as u8, SIDE as u8, 0, 0]);
    push_u16(&mut icon, 1);
    push_u16(&mut icon, 32);
    push_u32(&mut icon, image_bytes as u32);
    push_u32(&mut icon, 22);

    push_u32(&mut icon, 40);
    push_i32(&mut icon, SIDE as i32);
    push_i32(&mut icon, (SIDE * 2) as i32);
    push_u16(&mut icon, 1);
    push_u16(&mut icon, 32);
    push_u32(&mut icon, 0);
    push_u32(&mut icon, pixel_bytes as u32);
    push_i32(&mut icon, 0);
    push_i32(&mut icon, 0);
    push_u32(&mut icon, 0);
    push_u32(&mut icon, 0);

    let mut alpha = [[0_u8; SIDE]; SIDE];
    for (row_from_bottom, alpha_row) in alpha.iter_mut().rev().enumerate() {
        let y = SIDE - 1 - row_from_bottom;
        for (x, alpha_pixel) in alpha_row.iter_mut().enumerate() {
            let back = (2..=10).contains(&x) && (2..=9).contains(&y);
            let back_border = back && (x == 2 || x == 10 || y == 2 || y == 9);
            let front = (6..=14).contains(&x) && (6..=13).contains(&y);
            let front_border = front && (x == 6 || x == 14 || y == 6 || y == 13);
            let stand = (x == 8 || x == 9) && (14..=15).contains(&y);
            let opaque = back || front || stand;
            *alpha_pixel = if opaque { 255 } else { 0 };
            let (red, green, blue) = if front_border || back_border || stand {
                (84, 229, 219)
            } else if front {
                (18, 38, 48)
            } else if back {
                (25, 31, 43)
            } else {
                (0, 0, 0)
            };
            icon.extend_from_slice(&[blue, green, red, *alpha_pixel]);
        }
    }

    for alpha_row in alpha.iter().rev() {
        let mut row = [0_u8; MASK_ROW_BYTES];
        for (x, alpha_pixel) in alpha_row.iter().enumerate() {
            if *alpha_pixel == 0 {
                row[x / 8] |= 1 << (7 - (x % 8));
            }
        }
        icon.extend_from_slice(&row);
    }
    icon
}

fn push_u16(output: &mut Vec<u8>, value: u16) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn push_i32(output: &mut Vec<u8>, value: i32) {
    output.extend_from_slice(&value.to_le_bytes());
}
