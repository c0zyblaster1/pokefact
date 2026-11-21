use std::env;
use std::fs;
use std::io::{self, Cursor, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use colored::*;
use image::codecs::gif::GifDecoder;
use image::{imageops::FilterType, AnimationDecoder, DynamicImage, GenericImageView, Rgba, RgbaImage};
use serde_json::Value;

use crate::{center_str, fetch_bytes, term_width, theme_for_types, Rgb, Theme};

fn render_image_direct(img: &DynamicImage, silhouette: bool, alpha_thresh: u8) -> (usize, Vec<String>) {
    let (fw, fh) = img.dimensions();
    let mut rows = Vec::new();

    for y in (0..fh).step_by(2) {
        let mut line = String::new();
        for x in 0..fw {
            let top = img.get_pixel(x, y);
            let bottom = if y + 1 < fh {
                img.get_pixel(x, y + 1)
            } else {
                Rgba([0, 0, 0, 0])
            };

            let (tr, tg, tb) = if silhouette { (45, 52, 70) } else { (top[0], top[1], top[2]) };
            let (br, bg, bb) = if silhouette { (45, 52, 70) } else { (bottom[0], bottom[1], bottom[2]) };

            match (top[3] > alpha_thresh, bottom[3] > alpha_thresh) {
                (false, false) => line.push(' '),
                (true, false) => line.push_str(&format!("\x1b[38;2;{};{};{}m▀\x1b[0m", tr, tg, tb)),
                (false, true) => line.push_str(&format!("\x1b[38;2;{};{};{}m▄\x1b[0m", br, bg, bb)),
                (true, true) => line.push_str(&format!(
                    "\x1b[38;2;{};{};{};48;2;{};{};{}m▀\x1b[0m",
                    tr, tg, tb, br, bg, bb
                )),
            }
        }
        rows.push(line);
    }
    (fw as usize, rows)
}

pub fn render_rgba_lines(img: &DynamicImage, max_width: u32, silhouette: bool) -> (usize, Vec<String>) {
    let (w, h) = img.dimensions();
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (w, h, 0, 0);
    for (x, y, pixel) in img.pixels() {
        if pixel[3] > 20 {
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
        }
    }

    let cropped = if max_x >= min_x && max_y >= min_y {
        img.crop_imm(min_x, min_y, (max_x - min_x) + 1, (max_y - min_y) + 1)
    } else {
        img.clone()
    };

    let (cw, ch) = cropped.dimensions();
    let final_img = if cw > max_width {
        let new_h = ((ch as f32 / cw as f32) * max_width as f32).round().max(2.0) as u32;
        cropped.resize_exact(max_width, new_h, FilterType::Nearest)
    } else {
        cropped
    };

    render_image_direct(&final_img, silhouette, 25)
}

pub fn render_sprite_lines(img_bytes: &[u8], max_width: u32, silhouette: bool) -> (usize, Vec<String>) {
    match image::load_from_memory(img_bytes) {
        Ok(i) => render_rgba_lines(&i, max_width, silhouette),
        Err(_) => (14, vec!["[Sprite Error]".to_string()]),
    }
}

pub fn print_sprite_grid(sprites: &[(String, usize, Vec<String>)], theme: Theme) {
    if sprites.is_empty() {
        return;
    }
    let tw = term_width();
    let max_sprite_w = sprites.iter().map(|(l, w, _)| (*w).max(l.chars().count())).max().unwrap_or(36);
    let cell_w = max_sprite_w.max(26);
    let gap = 4;
    let stride = cell_w + gap;
    let cols = ((tw.saturating_sub(2)) / stride).max(1);

    for chunk in sprites.chunks(cols) {
        print!("  ");
        for (i, (label, _, _)) in chunk.iter().enumerate() {
            let centered = center_str(label, cell_w);
            let styled = if i % 2 == 0 {
                centered.truecolor(theme.primary.0, theme.primary.1, theme.primary.2).bold()
            } else {
                centered.truecolor(theme.secondary.0, theme.secondary.1, theme.secondary.2).bold()
            };
            print!("{}{}", styled, " ".repeat(gap));
        }
        println!();

        let max_h = chunk.iter().map(|(_, _, lines)| lines.len()).max().unwrap_or(0);
        for row_i in 0..max_h {
            print!("  ");
            for (_, sw, lines) in chunk {
                let top_pad = max_h - lines.len();
                if row_i < top_pad {
                    print!("{}", " ".repeat(cell_w));
                } else {
                    let left_p = (cell_w.saturating_sub(*sw)) / 2;
                    let right_p = cell_w.saturating_sub(*sw + left_p);
                    print!("{}{}{}", " ".repeat(left_p), lines[row_i - top_pad], " ".repeat(right_p));
                }
                print!("{}", " ".repeat(gap));
            }
            println!();
        }
        println!();
    }
}

fn current_term_dims() -> (usize, usize) {
    terminal_size::terminal_size()
        .map(|(terminal_size::Width(w), terminal_size::Height(h))| (w as usize, h as usize))
        .unwrap_or((80, 24))
}

pub fn play_animated_sprite(data: &Value, infinite: bool) {
    let name = data["name"].as_str().unwrap_or("UNKNOWN");
    let slug: String = name.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect();
    let showdown_gif = format!("https://play.pokemonshowdown.com/sprites/ani/{}.gif", slug);
    let gen5_gif = data["animated_sprite"].as_str().unwrap_or("");

    let gif_bytes = fetch_bytes(&showdown_gif).or_else(|| fetch_bytes(gen5_gif));
    let Some(bytes) = gif_bytes else {
        eprintln!("  {}", "No animated sprite available for this Pokemon.".yellow());
        return;
    };

    let Ok(decoder) = GifDecoder::new(Cursor::new(bytes)) else {
        eprintln!("  {}", "Failed to decode animated GIF.".red());
        return;
    };
    let Ok(frames) = decoder.into_frames().collect_frames() else {
        eprintln!("  {}", "Failed to extract GIF frames.".red());
        return;
    };
    if frames.is_empty() {
        return;
    }

    let types: Vec<String> = data["types"].as_array().unwrap_or(&vec![]).iter().filter_map(|v| v.as_str().map(String::from)).collect();
    let theme = theme_for_types(&types);

    let mut raw_frames: Vec<(DynamicImage, u64)> = Vec::with_capacity(frames.len());
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (u32::MAX, u32::MAX, 0u32, 0u32);

    for f in frames {
        let (num, den) = f.delay().numer_denom_ms();
        let delay_ms = if den == 0 { 80 } else { ((num / den) as u64).clamp(40, 220) };
        let dyn_img = DynamicImage::ImageRgba8(f.into_buffer());
        for (x, y, p) in dyn_img.pixels() {
            if p[3] > 20 {
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
        }
        raw_frames.push((dyn_img, delay_ms));
    }

    let (crop_x, crop_y, crop_w, crop_h) = if max_x >= min_x && max_y >= min_y {
        (min_x, min_y, (max_x - min_x) + 1, (max_y - min_y) + 1)
    } else {
        let (w, h) = raw_frames[0].0.dimensions();
        (0, 0, w.max(1), h.max(1))
    };

    let cropped_frames: Vec<(DynamicImage, u64)> = raw_frames
        .into_iter()
        .map(|(img, d)| (img.crop_imm(crop_x, crop_y, crop_w, crop_h), d))
        .collect();

    let header_txt = if infinite {
        format!("{} (infinite | Ctrl+C to stop)", name)
    } else {
        format!("{} (10s | Ctrl+C to stop)", name)
    };

    let build_for_size = |tw: usize, th: usize| -> (Vec<(Vec<String>, u64)>, String, usize, String) {
        let max_cols = tw.saturating_sub(2).clamp(16, 96) as u32;
        let max_rows = th.saturating_sub(3).clamp(8, 52) as u32;
        let max_px_h = max_rows * 2;

        let scale = (max_cols as f32 / crop_w as f32)
            .min(max_px_h as f32 / crop_h as f32)
            .min(1.0);
        let target_w = ((crop_w as f32 * scale).round() as u32).clamp(2, max_cols);
        let target_h = ((crop_h as f32 * scale).round() as u32).clamp(2, max_px_h);
        let downscaled = scale < 0.99;
        let alpha_thresh = if downscaled { 95 } else { 25 };

        let mut rendered = Vec::with_capacity(cropped_frames.len());
        for (img, delay) in &cropped_frames {
            let resized = if downscaled {
                img.resize_exact(target_w, target_h, FilterType::Lanczos3)
            } else {
                img.clone()
            };
            let (_, lines) = render_image_direct(&resized, false, alpha_thresh);
            rendered.push((lines, *delay));
        }

        let sprite_rows = rendered.first().map(|(l, _)| l.len()).unwrap_or(0);
        let left_pad = " ".repeat(tw.saturating_sub(target_w as usize) / 2);
        let top_pad = th.saturating_sub(sprite_rows + 3) / 2;
        let header_line = center_str(&header_txt, tw)
            .truecolor(theme.primary.0, theme.primary.1, theme.primary.2)
            .bold()
            .to_string();

        (rendered, left_pad, top_pad, header_line)
    };

    let (mut tw, mut th) = current_term_dims();
    let (mut rendered, mut left_pad, mut top_pad, mut header_line) = build_for_size(tw, th);

    let running = Arc::new(AtomicBool::new(true));
    let r_clone = Arc::clone(&running);
    let _ = ctrlc::set_handler(move || {
        r_clone.store(false, Ordering::SeqCst);
    });

    let mut stdout = io::stdout();
    let _ = write!(stdout, "\x1b[?1049h\x1b[?25l\x1b[2J");
    let _ = stdout.flush();

    let start = Instant::now();
    let limit = Duration::from_secs(10);
    let mut idx = 0usize;

    while running.load(Ordering::SeqCst) {
        if !infinite && start.elapsed() >= limit {
            break;
        }

        let (cur_tw, cur_th) = current_term_dims();
        if cur_tw != tw || cur_th != th {
            tw = cur_tw;
            th = cur_th;
            let rebuilt = build_for_size(tw, th);
            rendered = rebuilt.0;
            left_pad = rebuilt.1;
            top_pad = rebuilt.2;
            header_line = rebuilt.3;
        }

        let (lines, delay) = &rendered[idx];
        let mut buf = String::with_capacity(8192);
        buf.push_str("\x1b[H");
        buf.push_str(&header_line);
        buf.push_str("\x1b[K\n");
        for _ in 0..top_pad {
            buf.push_str("\x1b[K\n");
        }
        for line in lines {
            buf.push_str(&left_pad);
            buf.push_str(line);
            buf.push_str("\x1b[K\n");
        }
        buf.push_str("\x1b[J");
        let _ = stdout.write_all(buf.as_bytes());
        let _ = stdout.flush();

        let frame_start = Instant::now();
        let frame_dur = Duration::from_millis(*delay);
        while running.load(Ordering::SeqCst) && frame_start.elapsed() < frame_dur {
            if !infinite && start.elapsed() >= limit {
                break;
            }
            thread::sleep(Duration::from_millis(15));
        }
        idx = (idx + 1) % rendered.len();
    }

    let _ = write!(stdout, "\x1b[?25h\x1b[?1049l");
    let _ = stdout.flush();
}

pub fn render_hd_artwork(data: &Value) -> bool {
    let url = data["hd_artwork"]
        .as_str()
        .filter(|s| !s.is_empty())
        .or_else(|| data["normal_sprite"].as_str())
        .unwrap_or("");
    let Some(bytes) = fetch_bytes(url) else {
        return false;
    };
    let Ok(img) = image::load_from_memory(&bytes) else {
        return false;
    };

    let term = env::var("TERM").unwrap_or_default().to_lowercase();
    let term_prog = env::var("TERM_PROGRAM").unwrap_or_default().to_lowercase();
    let is_kitty = env::var("KITTY_WINDOW_ID").is_ok()
        || env::var("WEZTERM_EXECUTABLE").is_ok()
        || term.contains("kitty")
        || term_prog.contains("wezterm")
        || term_prog.contains("ghostty");
    let is_sixel = env::var("WT_SESSION").is_ok()
        || term.contains("sixel")
        || term.contains("foot")
        || term_prog.contains("iterm")
        || term_prog.contains("mintty");

    if is_kitty {
        let resized = img.resize(340, 340, FilterType::Lanczos3);
        let mut png_buf = Vec::new();
        if resized.write_to(&mut Cursor::new(&mut png_buf), image::ImageFormat::Png).is_ok() {
            let b64 = STANDARD.encode(&png_buf);
            let chunks: Vec<&[u8]> = b64.as_bytes().chunks(4096).collect();
            for (i, chunk) in chunks.iter().enumerate() {
                let more = if i + 1 < chunks.len() { 1 } else { 0 };
                let s = std::str::from_utf8(chunk).unwrap_or("");
                if i == 0 {
                    print!("\x1b_Ga=T,f=100,m={};{}\x1b\\", more, s);
                } else {
                    print!("\x1b_Gm={};{}\x1b\\", more, s);
                }
            }
            println!("\n");
            return true;
        }
    }

    if is_sixel {
        let resized = img.resize(280, 280, FilterType::Lanczos3).to_rgba8();
        let (w, h) = resized.dimensions();
        let mut out = String::with_capacity((w * h) as usize);
        out.push_str("\x1bPq");
        for r in 0..6u32 {
            for g in 0..6u32 {
                for b in 0..6u32 {
                    let idx = r * 36 + g * 6 + b;
                    out.push_str(&format!("#{};2;{};{};{}", idx, r * 20, g * 20, b * 20));
                }
            }
        }
        for band_y in (0..h).step_by(6) {
            let mut used_colors = [false; 216];
            let mut color_rows: Vec<Vec<(u32, u8)>> = vec![Vec::new(); 216];
            for x in 0..w {
                let mut col_masks: [u8; 216] = [0; 216];
                for bit in 0..6u32 {
                    let y = band_y + bit;
                    if y >= h { break; }
                    let p = resized.get_pixel(x, y);
                    if p[3] < 40 { continue; }
                    let r = ((p[0] as u32 * 5 + 127) / 255) as usize;
                    let g = ((p[1] as u32 * 5 + 127) / 255) as usize;
                    let b = ((p[2] as u32 * 5 + 127) / 255) as usize;
                    let idx = r * 36 + g * 6 + b;
                    col_masks[idx] |= 1 << bit;
                    used_colors[idx] = true;
                }
                for c in 0..216 {
                    if col_masks[c] != 0 {
                        color_rows[c].push((x, col_masks[c]));
                    }
                }
            }

            let mut first = true;
            for c in 0..216 {
                if !used_colors[c] { continue; }
                if !first { out.push('$'); }
                first = false;
                out.push_str(&format!("#{}", c));
                let mut cursor_x = 0u32;
                for &(x, mask) in &color_rows[c] {
                    if x > cursor_x {
                        let gap = x - cursor_x;
                        if gap > 3 {
                            out.push_str(&format!("!{}?", gap));
                        } else {
                            for _ in 0..gap { out.push('?'); }
                        }
                    }
                    out.push((63 + mask) as char);
                    cursor_x = x + 1;
                }
            }
            out.push('-');
        }
        out.push_str("\x1b\\");
        print!("  {}\n\n", out);
        let _ = io::stdout().flush();
        return true;
    }

    let (w, lines) = render_rgba_lines(&img, 72, false);
    let types: Vec<String> = data["types"].as_array().unwrap_or(&vec![]).iter().filter_map(|v| v.as_str().map(String::from)).collect();
    print_sprite_grid(&[("ARTWORK".to_string(), w, lines)], theme_for_types(&types));
    true
}

fn glyph_5x7(ch: char) -> [u8; 7] {
    match ch.to_ascii_uppercase() {
        'A' => [0x0E, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'B' => [0x1E, 0x11, 0x11, 0x1E, 0x11, 0x11, 0x1E],
        'C' => [0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E],
        'D' => [0x1C, 0x12, 0x11, 0x11, 0x11, 0x12, 0x1C],
        'E' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F],
        'F' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10],
        'G' => [0x0E, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0E],
        'H' => [0x11, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'I' => [0x0E, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E],
        'J' => [0x07, 0x02, 0x02, 0x02, 0x12, 0x12, 0x0C],
        'K' => [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11],
        'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F],
        'M' => [0x11, 0x1B, 0x15, 0x15, 0x11, 0x11, 0x11],
        'N' => [0x11, 0x19, 0x15, 0x13, 0x11, 0x11, 0x11],
        'O' => [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'P' => [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10],
        'Q' => [0x0E, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0D],
        'R' => [0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11],
        'S' => [0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E],
        'T' => [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'V' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x0A, 0x04],
        'W' => [0x11, 0x11, 0x11, 0x15, 0x15, 0x1B, 0x11],
        'X' => [0x11, 0x11, 0x0A, 0x04, 0x0A, 0x11, 0x11],
        'Y' => [0x11, 0x11, 0x0A, 0x04, 0x04, 0x04, 0x04],
        'Z' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1F],
        '0' => [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E],
        '1' => [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
        '2' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F],
        '3' => [0x1E, 0x01, 0x01, 0x0E, 0x01, 0x01, 0x1E],
        '4' => [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
        '5' => [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E],
        '6' => [0x0E, 0x10, 0x10, 0x1E, 0x11, 0x11, 0x0E],
        '7' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E],
        '9' => [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x01, 0x0E],
        '#' => [0x0A, 0x1F, 0x0A, 0x0A, 0x1F, 0x0A, 0x00],
        ':' => [0x00, 0x04, 0x04, 0x00, 0x04, 0x04, 0x00],
        '/' => [0x01, 0x02, 0x02, 0x04, 0x08, 0x08, 0x10],
        '-' => [0x00, 0x00, 0x00, 0x1F, 0x00, 0x00, 0x00],
        '.' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x0C],
        ',' => [0x00, 0x00, 0x00, 0x00, 0x04, 0x04, 0x08],
        '(' => [0x02, 0x04, 0x08, 0x08, 0x08, 0x04, 0x02],
        ')' => [0x08, 0x04, 0x02, 0x02, 0x02, 0x04, 0x08],
        '%' => [0x18, 0x19, 0x02, 0x04, 0x08, 0x13, 0x03],
        '!' => [0x04, 0x04, 0x04, 0x04, 0x04, 0x00, 0x04],
        '|' => [0x04, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        _   => [0x00; 7],
    }
}

fn draw_text(img: &mut RgbaImage, text: &str, x: u32, y: u32, scale: u32, col: Rgb) {
    let mut cx = x;
    let rgba = Rgba([col.0, col.1, col.2, 255]);
    for ch in text.chars() {
        let rows = glyph_5x7(ch);
        for (ry, &bits) in rows.iter().enumerate() {
            for rx in 0..5u32 {
                if (bits >> (4 - rx)) & 1 == 1 {
                    for sy in 0..scale {
                        for sx in 0..scale {
                            let px = cx + rx * scale + sx;
                            let py = y + (ry as u32) * scale + sy;
                            if px < img.width() && py < img.height() {
                                img.put_pixel(px, py, rgba);
                            }
                        }
                    }
                }
            }
        }
        cx += 6 * scale;
    }
}

fn fill_rect(img: &mut RgbaImage, x: u32, y: u32, w: u32, h: u32, col: Rgb) {
    let rgba = Rgba([col.0, col.1, col.2, 255]);
    for py in y..(y + h).min(img.height()) {
        for px in x..(x + w).min(img.width()) {
            img.put_pixel(px, py, rgba);
        }
    }
}

fn resolve_export_path(default_filename: &str, preset: Option<&str>) -> PathBuf {
    let raw_input = if let Some(p) = preset {
        p.trim().to_string()
    } else {
        print!("  Save card to (folder or .png path, Enter for current dir): ");
        let _ = io::stdout().flush();
        let mut buf = String::new();
        let _ = io::stdin().read_line(&mut buf);
        buf.trim().trim_matches('"').trim_matches('\'').to_string()
    };

    if raw_input.is_empty() {
        return env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(default_filename);
    }

    let expanded = if raw_input == "~" {
        dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
    } else if let Some(rest) = raw_input.strip_prefix("~/").or_else(|| raw_input.strip_prefix("~\\")) {
        dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(rest)
    } else {
        PathBuf::from(&raw_input)
    };

    let looks_like_png = expanded
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("png"))
        .unwrap_or(false);

    if expanded.is_dir() || raw_input.ends_with('/') || raw_input.ends_with('\\') || !looks_like_png {
        let _ = fs::create_dir_all(&expanded);
        expanded.join(default_filename)
    } else {
        if let Some(parent) = expanded.parent() {
            if !parent.as_os_str().is_empty() {
                let _ = fs::create_dir_all(parent);
            }
        }
        expanded
    }
}

pub fn export_pokemon_card(data: &Value, preset_dest: Option<&str>) {
    let name = data["name"].as_str().unwrap_or("POKEMON");
    let id = data["id"].as_i64().unwrap_or(0);
    let types: Vec<String> = data["types"].as_array().unwrap_or(&vec![]).iter().filter_map(|v| v.as_str().map(String::from)).collect();
    let theme = theme_for_types(&types);

    let (w, h) = (960u32, 540u32);
    let mut canvas = RgbaImage::from_pixel(w, h, Rgba([16, 18, 26, 255]));

    fill_rect(&mut canvas, 0, 0, w, 6, theme.primary);
    fill_rect(&mut canvas, 0, h - 6, w, 6, theme.secondary);
    fill_rect(&mut canvas, 0, 0, 6, h, theme.primary);
    fill_rect(&mut canvas, w - 6, 0, 6, h, theme.secondary);
    fill_rect(&mut canvas, 20, 20, w - 40, 64, (24, 28, 40));

    let title = format!("#{:04} {} - {}", id, name, types.join(" / "));
    draw_text(&mut canvas, &title, 38, 38, 4, theme.primary);

    let art_url = data["hd_artwork"]
        .as_str()
        .filter(|s| !s.is_empty())
        .or_else(|| data["normal_sprite"].as_str())
        .unwrap_or("");
    if let Some(bytes) = fetch_bytes(art_url) {
        if let Ok(art) = image::load_from_memory(&bytes) {
            let resized = art.resize(340, 340, FilterType::Lanczos3).to_rgba8();
            let ox = 40 + (340 - resized.width()) / 2;
            let oy = 115 + (340 - resized.height()) / 2;
            for (px, py, p) in resized.enumerate_pixels() {
                if p[3] > 25 && ox + px < w && oy + py < h {
                    canvas.put_pixel(ox + px, oy + py, *p);
                }
            }
        }
    }

    let rx = 415u32;
    let evo_raw = data["evolution"].as_str().unwrap_or("");
    let evo_clean: String = evo_raw.chars().take(36).collect();
    let hw = format!(
        "HT: {}M  WT: {}KG  GEN: {}",
        data["height"].as_f64().unwrap_or(0.0),
        data["weight"].as_f64().unwrap_or(0.0),
        data["generation"].as_str().unwrap_or("")
    );
    draw_text(&mut canvas, &hw, rx, 115, 2, (210, 215, 230));
    draw_text(&mut canvas, &format!("EVO: {}", evo_clean), rx, 145, 2, theme.secondary);

    let mut sy = 190u32;
    let mut bst = 0i32;
    if let Some(stats) = data["stats"].as_array() {
        for s in stats {
            let sname = s[0].as_str().unwrap_or("");
            let val = s[1].as_i64().unwrap_or(0) as i32;
            bst += val;
            draw_text(&mut canvas, &format!("{:<8} {:>3}", sname, val), rx, sy, 2, (235, 235, 240));
            let bar_x = rx + 165;
            let bar_max = 330u32;
            let bar_fill = (((val as f32 / 255.0) * bar_max as f32).round() as u32).min(bar_max);
            fill_rect(&mut canvas, bar_x, sy, bar_max, 14, (36, 40, 54));
            let bar_col = if val >= 115 { (80, 240, 255) } else if val >= 90 { (90, 235, 115) } else if val >= 65 { (255, 210, 75) } else { (255, 95, 95) };
            fill_rect(&mut canvas, bar_x, sy, bar_fill, 14, bar_col);
            sy += 34;
        }
    }

    draw_text(&mut canvas, &format!("BASE STAT TOTAL: {}", bst), rx, sy + 14, 3, (255, 230, 110));

    let default_name = format!("pokefact_{}.png", name.to_lowercase());
    let out_path = resolve_export_path(&default_name, preset_dest);
    match canvas.save(&out_path) {
        Ok(_) => {
            let full = fs::canonicalize(&out_path).unwrap_or(out_path);
            let display = full.to_string_lossy().replace("\\\\?\\", "");
            println!("  {}\n", format!("Saved card to: {}", display).green().bold());
        }
        Err(e) => eprintln!("  Failed to save PNG: {}", e),
    }
}
