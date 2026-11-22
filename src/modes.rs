use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
#[cfg(any(windows, target_os = "macos", target_os = "linux"))]
use std::process::Command;
use std::thread;
use chrono::{Datelike, Local, NaiveDate};
use colored::*;
use indicatif::{ProgressBar, ProgressStyle};
use regex::Regex;
use serde_json::Value;
use sysinfo::System;

use crate::battle::fetch_move_detail;
use crate::graphics::{print_sprite_grid, render_sprite_lines};
use crate::{
    best_stab_multiplier, center_str, defensive_multipliers, detect_archetype, display_pokemon,
    fetch_bytes, fetch_pokemon, fuzzy_match, get_stat, hp_bar, pad_left, pad_right, rand_seed,
    signature_move_for_type, spawn_play_cry, term_width, theme_for_types, type_matchups, type_rgb,
    wrap_text, Capitalize, Rgb, Theme,
};

fn box_path() -> PathBuf {
    let mut dir = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    dir.push(".pokefact_box.json");
    dir
}

fn daily_path() -> PathBuf {
    let mut dir = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    dir.push(".pokefact_daily.json");
    dir
}

pub fn load_box_names() -> Vec<String> {
    let list: Vec<String> = fs::read_to_string(box_path())
        .ok()
        .and_then(|c| serde_json::from_str(&c).ok())
        .unwrap_or_default();
    list.into_iter()
        .filter_map(|entry| entry.split_whitespace().nth(1).map(|s| s.to_lowercase()))
        .collect()
}

pub fn run_calc_mode(p1_name: &str, move_name: &str, p2_name: &str) {
    let (d1, d2, mv) = thread::scope(|s| {
        let h1 = s.spawn(|| fetch_pokemon(p1_name, false));
        let h2 = s.spawn(|| fetch_pokemon(p2_name, false));
        let hm = s.spawn(|| fetch_move_detail(move_name));
        (h1.join().ok().flatten(), h2.join().ok().flatten(), hm.join().ok().flatten())
    });

    let (Some(d1), Some(d2), Some(mv)) = (d1, d2, mv) else {
        eprintln!("  {}", "Usage: pokefact --calc <attacker> \"<move>\" <defender>".red().bold());
        return;
    };

    let tw = term_width().saturating_sub(4).max(28);
    let n1 = d1["name"].as_str().unwrap();
    let n2 = d2["name"].as_str().unwrap();
    let t1: Vec<String> = d1["types"].as_array().unwrap().iter().filter_map(|v| v.as_str().map(String::from)).collect();
    let t2: Vec<String> = d2["types"].as_array().unwrap().iter().filter_map(|v| v.as_str().map(String::from)).collect();
    let s1: Vec<(String, i32)> = d1["stats"].as_array().unwrap().iter().map(|s| (s[0].as_str().unwrap().to_string(), s[1].as_i64().unwrap() as i32)).collect();
    let s2: Vec<(String, i32)> = d2["stats"].as_array().unwrap().iter().map(|s| (s[0].as_str().unwrap().to_string(), s[1].as_i64().unwrap() as i32)).collect();

    if mv.is_status || mv.power <= 0 {
        println!("\n  {} is a Status move (0 BP).\n", mv.name.yellow().bold());
        return;
    }

    let def_mults = defensive_multipliers(&t2);
    let eff = def_mults.iter().find(|(k, _)| k.eq_ignore_ascii_case(&mv.mtype)).map(|(_, &m)| m).unwrap_or(1.0);
    let stab = if t1.iter().any(|t| t.eq_ignore_ascii_case(&mv.mtype)) { 1.5 } else { 1.0 };
    let mc = type_rgb(&mv.mtype);

    println!();
    let head = format!("DAMAGE CALC: {}'s {} ({} | {} BP) -> {}", n1, mv.name, mv.mtype, mv.power, n2);
    for line in wrap_text(&head, tw) {
        println!("  {}", line.truecolor(mc.0, mc.1, mc.2).bold());
    }
    let meta = format!("STAB: {}x | Eff: {}x | {}", stab, eff, if mv.is_physical { "Physical" } else { "Special" });
    for line in wrap_text(&meta, tw) {
        println!("  {}", line);
    }
    println!();

    for &lvl in &[50i32, 100i32] {
        let hp_max = ((2 * get_stat(&s2, "HP") + 31) * lvl) / 100 + lvl + 10;
        let off_base = if mv.is_physical { get_stat(&s1, "Attack") } else { get_stat(&s1, "Sp. Atk") };
        let def_base = if mv.is_physical { get_stat(&s2, "Defense") } else { get_stat(&s2, "Sp. Def") };
        let atk = ((2 * off_base + 31) * lvl) / 100 + 5;
        let def = (((2 * def_base + 31) * lvl) / 100 + 5).max(1);

        let base = (((((2 * lvl) / 5 + 2) * mv.power * atk) / def) / 50) + 2;
        let min_d = if eff == 0.0 { 0 } else { ((base as f32) * stab * eff * 0.85).floor().max(1.0) as i32 };
        let max_d = if eff == 0.0 { 0 } else { ((base as f32) * stab * eff * 1.00).floor().max(1.0) as i32 };
        let min_p = (min_d as f32 / hp_max as f32) * 100.0;
        let max_p = (max_d as f32 / hp_max as f32) * 100.0;

        let verdict = if eff == 0.0 {
            "IMMUNE (0%)"
        } else if min_p >= 100.0 {
            "GUARANTEED OHKO"
        } else if max_p >= 100.0 {
            "POSSIBLE OHKO"
        } else if min_p >= 50.0 {
            "GUARANTEED 2HKO"
        } else if max_p >= 50.0 {
            "POSSIBLE 2HKO"
        } else if min_p >= 33.4 {
            "GUARANTEED 3HKO"
        } else {
            "4HKO+"
        };

        let summary = format!("Lv.{:<3} | {}-{} HP ({:.1}%-{:.1}% of {})", lvl, min_d, max_d, min_p, max_p, hp_max);
        if summary.len() + verdict.len() + 6 <= tw {
            println!("  {}  =>  {}", summary, verdict.truecolor(255, 220, 75).bold());
        } else {
            println!("  {}", summary);
            println!("         => {}", verdict.truecolor(255, 220, 75).bold());
        }
    }
    println!();
}

pub fn run_type_sandbox(types: &[String]) {
    if types.is_empty() {
        eprintln!("  {}", "Usage: pokefact --type fire dragon".red());
        return;
    }
    let tw = term_width().saturating_sub(4).max(28);
    let clean: Vec<String> = types.iter().take(2).map(|t| t.capitalize()).collect();
    let theme = theme_for_types(&clean);
    let (x4, x2, h, q, imm) = type_matchups(&clean);

    println!("\n  {}\n", format!("TYPE CHART: {}", clean.join(" / ").to_uppercase()).truecolor(theme.primary.0, theme.primary.1, theme.primary.2).bold());
    let print_row = |label: &str, items: &[String], lc: Rgb, vc: Rgb| {
        if items.is_empty() { return; }
        let val_w = tw.saturating_sub(12).max(14);
        for (i, line) in wrap_text(&items.join(", "), val_w).iter().enumerate() {
            let l = pad_right(if i == 0 { label } else { "" }, 12).truecolor(lc.0, lc.1, lc.2).bold();
            println!("  {}{}", l, line.truecolor(vc.0, vc.1, vc.2));
        }
    };
    print_row("4x WEAK:", &x4, (255, 65, 65), (255, 110, 110));
    print_row("2x WEAK:", &x2, (255, 145, 75), (255, 185, 120));
    print_row("0x IMMUNE:", &imm, (110, 235, 255), (160, 245, 255));
    print_row("0.25x RES:", &q, (80, 240, 140), (130, 245, 175));
    print_row("0.5x RES:", &h, (95, 225, 125), (150, 235, 170));
    println!();
}

fn detect_gpu() -> String {
    #[cfg(windows)]
    {
        if let Ok(out) = Command::new("powershell")
            .args(["-NoProfile", "-Command", "(Get-CimInstance Win32_VideoController | Select-Object -First 1 -ExpandProperty Name)"])
            .output()
        {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !s.is_empty() { return s; }
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Ok(out) = Command::new("system_profiler").args(["SPDisplaysDataType"]).output() {
            for line in String::from_utf8_lossy(&out.stdout).lines() {
                if let Some(val) = line.trim().strip_prefix("Chipset Model:") {
                    return val.trim().to_string();
                }
            }
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(out) = Command::new("sh").args(["-c", "lspci | grep -iE 'vga|3d' | head -n1 | cut -d: -f3"]).output() {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !s.is_empty() { return s; }
        }
    }
    "Mobile / Integrated GPU".to_string()
}

pub fn run_fetch_mode(query: Option<String>) {
    let target = query.unwrap_or_else(|| ((rand_seed() % 1025) + 1).to_string());
    let Some(data) = fetch_pokemon(&target, false) else {
        eprintln!("  Could not load Pokemon '{}'.", target);
        return;
    };

    let mut sys = System::new_all();
    sys.refresh_all();

    let tw = term_width();
    let side_by_side = tw >= 78;
    let sp_max = if side_by_side { 36 } else { (tw.saturating_sub(4) as u32).clamp(20, 38) };

    let name = data["name"].as_str().unwrap_or("POKEMON");
    let types: Vec<String> = data["types"].as_array().unwrap_or(&vec![]).iter().filter_map(|v| v.as_str().map(String::from)).collect();
    let theme = theme_for_types(&types);

    let os_name = format!("{} {}", System::name().unwrap_or_else(|| "OS".into()), System::os_version().unwrap_or_default());
    let host = System::host_name().unwrap_or_else(|| "localhost".into());
    let cpu = sys.cpus().first().map(|c| c.brand().trim().to_string()).unwrap_or_else(|| "Unknown CPU".into());
    let cores = sys.cpus().len();
    let gpu = detect_gpu();
    let used_gb = sys.used_memory() as f64 / 1_073_741_824.0;
    let tot_gb = sys.total_memory() as f64 / 1_073_741_824.0;
    let up = System::uptime();
    let uptime_str = format!("{}h {}m", up / 3600, (up % 3600) / 60);

    let (sw, sprite_lines) = data["normal_sprite"]
        .as_str()
        .and_then(fetch_bytes)
        .map(|b| render_sprite_lines(&b, sp_max, false))
        .unwrap_or((10, vec!["[No Sprite]".into()]));

    let text_w = if side_by_side { tw.saturating_sub(sw + 8).max(24) } else { tw.saturating_sub(4).max(24) };
    let val_w = text_w.saturating_sub(9).max(12);
    let trunc = |s: String| -> String {
        if s.chars().count() > val_w { s.chars().take(val_w.saturating_sub(1)).collect::<String>() + "…" } else { s }
    };

    let info_lines = vec![
        trunc(format!("{}@{}", name.to_lowercase(), host)).truecolor(theme.primary.0, theme.primary.1, theme.primary.2).bold().to_string(),
        "─".repeat(text_w.min(36)).truecolor(theme.secondary.0, theme.secondary.1, theme.secondary.2).to_string(),
        format!("{} {}", "OS:     ".truecolor(theme.primary.0, theme.primary.1, theme.primary.2).bold(), trunc(os_name)),
        format!("{} {}", "CPU:    ".truecolor(theme.primary.0, theme.primary.1, theme.primary.2).bold(), trunc(format!("{} ({}t)", cpu, cores))),
        format!("{} {}", "GPU:    ".truecolor(theme.primary.0, theme.primary.1, theme.primary.2).bold(), trunc(gpu)),
        format!("{} {}", "Memory: ".truecolor(theme.primary.0, theme.primary.1, theme.primary.2).bold(), trunc(format!("{:.2} / {:.2} GiB", used_gb, tot_gb))),
        format!("{} {}", "Uptime: ".truecolor(theme.primary.0, theme.primary.1, theme.primary.2).bold(), uptime_str),
        format!("{} {}", "Partner:".truecolor(theme.primary.0, theme.primary.1, theme.primary.2).bold(), trunc(format!("#{} {} ({})", data["id"], name, types.join("/")))),
        String::new(),
        format!(
            "{}{}{}{}{}{}",
            "   ".on_truecolor(theme.primary.0, theme.primary.1, theme.primary.2),
            "   ".on_truecolor(theme.secondary.0, theme.secondary.1, theme.secondary.2),
            "   ".on_truecolor(80, 240, 255),
            "   ".on_truecolor(90, 235, 115),
            "   ".on_truecolor(255, 210, 75),
            "   ".on_truecolor(255, 95, 95),
        ),
    ];

    println!();
    if side_by_side {
        let max_rows = sprite_lines.len().max(info_lines.len());
        for i in 0..max_rows {
            let left = sprite_lines.get(i).cloned().unwrap_or_else(|| " ".repeat(sw));
            let right = info_lines.get(i).cloned().unwrap_or_default();
            println!("  {}    {}", left, right);
        }
    } else {
        let lp = " ".repeat(tw.saturating_sub(sw) / 2);
        for line in &sprite_lines {
            println!("{}{}", lp, line);
        }
        println!();
        for line in &info_lines {
            println!("  {}", line);
        }
    }
    println!();
}

pub fn run_banner_mode(query: Option<String>) {
    let target = query.unwrap_or_else(|| ((rand_seed() % 1025) + 1).to_string());
    let Some(data) = fetch_pokemon(&target, false) else { return; };

    let tw = term_width();
    let side_by_side = tw >= 66;
    let name = data["name"].as_str().unwrap_or("");
    let types: Vec<String> = data["types"].as_array().unwrap_or(&vec![]).iter().filter_map(|v| v.as_str().map(String::from)).collect();
    let theme = theme_for_types(&types);

    let sp_max = if side_by_side { 24 } else { (tw.saturating_sub(4) as u32).clamp(18, 28) };
    let (sw, sprite) = data["normal_sprite"]
        .as_str()
        .and_then(fetch_bytes)
        .map(|b| render_sprite_lines(&b, sp_max, false))
        .unwrap_or((8, vec![]));

    let fact = data["quiz_clues"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(|v| v.as_str())
        .unwrap_or("Ready for adventure!");

    let text_w = if side_by_side { tw.saturating_sub(sw + 7).max(20) } else { tw.saturating_sub(4).max(24) };
    let mut right = vec![
        format!("#{:04} {} [{}]", data["id"].as_i64().unwrap_or(0), name, types.join("/"))
            .truecolor(theme.primary.0, theme.primary.1, theme.primary.2)
            .bold()
            .to_string(),
        "─".repeat(text_w.min(42)).dimmed().to_string(),
    ];
    for line in wrap_text(fact, text_w) {
        right.push(line.truecolor(220, 225, 235).to_string());
    }

    println!();
    if side_by_side {
        let rows = sprite.len().max(right.len());
        for i in 0..rows {
            let l = sprite.get(i).cloned().unwrap_or_else(|| " ".repeat(sw));
            let r = right.get(i).cloned().unwrap_or_default();
            println!("  {}   {}", l, r);
        }
    } else {
        let lp = " ".repeat(tw.saturating_sub(sw) / 2);
        for l in &sprite { println!("{}{}", lp, l); }
        println!();
        for r in &right { println!("  {}", r); }
    }
    println!();
}

pub fn run_daily_mode(play_audio: bool, hd_mode: bool) {
    let today = Local::now().date_naive();
    let today_str = today.format("%Y-%m-%d").to_string();

    let seed = (today.year() as u64).wrapping_mul(10007)
        ^ (today.month() as u64).wrapping_mul(31337)
        ^ (today.day() as u64).wrapping_mul(99991);
    let daily_id = ((seed % 1025) + 1).to_string();

    let mut streak = 1u64;
    if let Ok(content) = fs::read_to_string(daily_path()) {
        if let Ok(json) = serde_json::from_str::<Value>(&content) {
            let last = json["last_date"].as_str().unwrap_or("");
            let prev_streak = json["streak"].as_u64().unwrap_or(0);
            if last == today_str {
                streak = prev_streak.max(1);
            } else if let Ok(prev_date) = NaiveDate::parse_from_str(last, "%Y-%m-%d") {
                if (today - prev_date).num_days() == 1 {
                    streak = prev_streak + 1;
                }
            }
        }
    }
    let _ = fs::write(
        daily_path(),
        serde_json::json!({ "last_date": today_str, "streak": streak }).to_string(),
    );

    let tw = term_width().saturating_sub(4).max(28);
    let head = format!("POKEMON OF THE DAY ({}) | STREAK: {} DAY(S)", today_str, streak);
    println!();
    for line in wrap_text(&head, tw) {
        println!("  {}", line.truecolor(255, 215, 70).bold());
    }

    if let Some(data) = fetch_pokemon(&daily_id, false) {
        display_pokemon(&data, false, 10, play_audio, hd_mode);
    }
}

fn short_stat_label(s: &str) -> &'static str {
    match s.to_lowercase().as_str() {
        "hp" => "HP",
        "attack" => "ATK",
        "defense" => "DEF",
        "sp. atk" => "SP.ATK",
        "sp. def" => "SP.DEF",
        "speed" => "SPEED",
        _ => "STAT",
    }
}

fn print_vs_wrapped(text: &str, col: Rgb, is_bold: bool, content_w: usize, gold: Rgb) {
    let bv = "│".truecolor(gold.0, gold.1, gold.2);
    for line in wrap_text(text, content_w) {
        let padded = pad_right(&line, content_w);
        let styled = if is_bold {
            padded.truecolor(col.0, col.1, col.2).bold()
        } else {
            padded.truecolor(col.0, col.1, col.2)
        };
        println!("  {} {} {}", bv, styled, bv);
    }
}

pub fn run_vs_mode(p1_name: &str, p2_name: &str, play_audio: bool) {
    let pb = ProgressBar::new_spinner();
    pb.set_style(ProgressStyle::default_spinner().template("{spinner:.yellow} Loading arena...").unwrap());
    pb.enable_steady_tick(std::time::Duration::from_millis(80));

    let (d1, d2) = thread::scope(|s| {
        let h1 = s.spawn(|| fetch_pokemon(p1_name, false));
        let h2 = s.spawn(|| fetch_pokemon(p2_name, false));
        (h1.join().ok().flatten(), h2.join().ok().flatten())
    });
    pb.finish_and_clear();

    let (Some(d1), Some(d2)) = (d1, d2) else {
        eprintln!("  {}", "Could not load one or both Pokemon for VS mode.".red().bold());
        return;
    };

    let tw = term_width();
    let inner_w = tw.saturating_sub(5).clamp(30, 86);
    let content_w = inner_w - 2;
    let sprite_max_w = (tw.saturating_sub(6) as u32).clamp(24, 56);

    let n1 = d1["name"].as_str().unwrap();
    let n2 = d2["name"].as_str().unwrap();
    let t1: Vec<String> = d1["types"].as_array().unwrap().iter().filter_map(|v| v.as_str().map(String::from)).collect();
    let t2: Vec<String> = d2["types"].as_array().unwrap().iter().filter_map(|v| v.as_str().map(String::from)).collect();

    let c1 = type_rgb(t1.first().map(|s| s.as_str()).unwrap_or("cyan"));
    let c2 = type_rgb(t2.first().map(|s| s.as_str()).unwrap_or("magenta"));
    let gold = (255, 215, 70);

    let audio_h = if play_audio {
        spawn_play_cry(d1["id"].as_i64().unwrap_or(0), n1.to_string(), d1["cry_url"].as_str().unwrap_or("").to_string())
    } else {
        None
    };

    let full_banner = format!("BATTLE ARENA: {} ({}) VS {} ({})", n1, t1.join("/"), n2, t2.join("/"));
    println!();
    for line in wrap_text(&full_banner, inner_w + 2) {
        println!("  {}", center_str(&line, inner_w + 2).truecolor(gold.0, gold.1, gold.2).bold());
    }
    println!();

    let sprites = thread::scope(|s| {
        let u1 = d1["normal_sprite"].as_str().unwrap_or("").to_string();
        let u2 = d2["normal_sprite"].as_str().unwrap_or("").to_string();
        let h1 = s.spawn(move || fetch_bytes(&u1).map(|b| { let (w, l) = render_sprite_lines(&b, sprite_max_w, false); (format!("[P1] {}", n1), w, l) }));
        let h2 = s.spawn(move || fetch_bytes(&u2).map(|b| { let (w, l) = render_sprite_lines(&b, sprite_max_w, false); (format!("[P2] {}", n2), w, l) }));
        let mut out = Vec::new();
        if let Ok(Some(x)) = h1.join() { out.push(x); }
        if let Ok(Some(x)) = h2.join() { out.push(x); }
        out
    });
    print_sprite_grid(&sprites, Theme { primary: c1, secondary: c2 });

    let s1: Vec<(String, i32)> = d1["stats"].as_array().unwrap().iter().map(|s| (s[0].as_str().unwrap().to_string(), s[1].as_i64().unwrap() as i32)).collect();
    let s2: Vec<(String, i32)> = d2["stats"].as_array().unwrap().iter().map(|s| (s[0].as_str().unwrap().to_string(), s[1].as_i64().unwrap() as i32)).collect();

    let border_v = "│".truecolor(gold.0, gold.1, gold.2);
    println!("  {}", format!("╭{}╮", "─".repeat(inner_w)).truecolor(gold.0, gold.1, gold.2));

    if content_w >= 66 {
        let side_w = (content_w - 6) / 2;
        let mid_w = content_w - side_w * 2;
        println!(
            "  {} {}{}{} {}",
            border_v,
            pad_right(&format!("{} ({})", n1, detect_archetype(&s1)), side_w).truecolor(c1.0, c1.1, c1.2).bold(),
            center_str("VS", mid_w).truecolor(gold.0, gold.1, gold.2).bold(),
            pad_left(&format!("{} ({})", n2, detect_archetype(&s2)), side_w).truecolor(c2.0, c2.1, c2.2).bold(),
            border_v
        );
    } else {
        let side_w = (content_w - 4) / 2;
        let mid_w = content_w - side_w * 2;
        println!(
            "  {} {}{}{} {}",
            border_v,
            pad_right(n1, side_w).truecolor(c1.0, c1.1, c1.2).bold(),
            center_str("VS", mid_w).truecolor(gold.0, gold.1, gold.2).bold(),
            pad_left(n2, side_w).truecolor(c2.0, c2.1, c2.2).bold(),
            border_v
        );
    }

    println!("  {}", format!("├{}┤", "─".repeat(inner_w)).truecolor(gold.0, gold.1, gold.2));

    let (mut bst1, mut bst2, mut wins1, mut wins2) = (0, 0, 0, 0);
    for i in 0..s1.len().min(s2.len()) {
        let (v1, v2) = (s1[i].1, s2[i].1);
        bst1 += v1; bst2 += v2;
        if v1 > v2 { wins1 += 1; } else if v2 > v1 { wins2 += 1; }
        let lbl = short_stat_label(&s1[i].0);

        if content_w >= 70 {
            let bar_w = ((content_w - 42) / 2).clamp(6, 14);
            let win_w = content_w - 24 - bar_w * 2;
            let f1 = ((v1 as f32 / 255.0) * bar_w as f32).round().min(bar_w as f32) as usize;
            let f2 = ((v2 as f32 / 255.0) * bar_w as f32).round().min(bar_w as f32) as usize;
            let b1 = format!("{}{}", "█".repeat(f1).truecolor(c1.0, c1.1, c1.2), "░".repeat(bar_w - f1).truecolor(55, 55, 65));
            let b2 = format!("{}{}", "█".repeat(f2).truecolor(c2.0, c2.1, c2.2), "░".repeat(bar_w - f2).truecolor(55, 55, 65));
            let win_txt = if v1 > v2 {
                pad_right(&format!("{} (+{})", n1, v1 - v2), win_w).truecolor(c1.0, c1.1, c1.2).bold()
            } else if v2 > v1 {
                pad_right(&format!("{} (+{})", n2, v2 - v1), win_w).truecolor(c2.0, c2.1, c2.2).bold()
            } else {
                pad_right("EVEN TIE", win_w).dimmed()
            };
            println!(
                "  {} {} {} {} vs {} {} │ {} {}",
                border_v,
                pad_right(lbl, 8).truecolor(235, 235, 240).bold(),
                if v1 > v2 { format!("{:>3}", v1).green().bold() } else { format!("{:>3}", v1).truecolor(180, 185, 195) },
                b1, b2,
                if v2 > v1 { format!("{:<3}", v2).green().bold() } else { format!("{:<3}", v2).truecolor(180, 185, 195) },
                win_txt, border_v
            );
        } else if content_w >= 42 {
            let bar_w = ((content_w - 20) / 2).clamp(4, 14);
            let extra = content_w - (20 + bar_w * 2);
            let f1 = ((v1 as f32 / 255.0) * bar_w as f32).round().min(bar_w as f32) as usize;
            let f2 = ((v2 as f32 / 255.0) * bar_w as f32).round().min(bar_w as f32) as usize;
            let b1 = format!("{}{}", "█".repeat(f1).truecolor(c1.0, c1.1, c1.2), "░".repeat(bar_w - f1).truecolor(55, 55, 65));
            let b2 = format!("{}{}", "█".repeat(f2).truecolor(c2.0, c2.1, c2.2), "░".repeat(bar_w - f2).truecolor(55, 55, 65));
            println!(
                "  {} {} {} {} vs {} {}{} {}",
                border_v,
                pad_right(lbl, 7).truecolor(235, 235, 240).bold(),
                if v1 > v2 { format!("{:>3}", v1).green().bold() } else { format!("{:>3}", v1).truecolor(180, 185, 195) },
                b1, b2,
                if v2 > v1 { format!("{:<3}", v2).green().bold() } else { format!("{:<3}", v2).truecolor(180, 185, 195) },
                " ".repeat(extra),
                border_v
            );
        } else {
            let diff_w = content_w.saturating_sub(19);
            let diff = if v1 > v2 {
                pad_right(&format!("P1 +{}", v1 - v2), diff_w).truecolor(c1.0, c1.1, c1.2).bold()
            } else if v2 > v1 {
                pad_right(&format!("P2 +{}", v2 - v1), diff_w).truecolor(c2.0, c2.1, c2.2).bold()
            } else {
                pad_right("TIE", diff_w).dimmed()
            };
            println!(
                "  {} {} {} vs {} {} {}",
                border_v,
                pad_right(lbl, 7).truecolor(235, 235, 240).bold(),
                if v1 > v2 { format!("{:>3}", v1).green().bold() } else { format!("{:>3}", v1).truecolor(180, 185, 195) },
                if v2 > v1 { format!("{:<3}", v2).green().bold() } else { format!("{:<3}", v2).truecolor(180, 185, 195) },
                diff,
                border_v
            );
        }
    }

    let (m1_type, m1_mult) = best_stab_multiplier(&t1, &t2);
    let (m2_type, m2_mult) = best_stab_multiplier(&t2, &t1);
    let max_hp1 = get_stat(&s1, "HP") * 2 + 110;
    let max_hp2 = get_stat(&s2, "HP") * 2 + 110;
    let (mut hp1, mut hp2) = (max_hp1, max_hp2);
    let phys1 = get_stat(&s1, "Attack") >= get_stat(&s1, "Sp. Atk");
    let phys2 = get_stat(&s2, "Attack") >= get_stat(&s2, "Sp. Atk");
    let move1 = signature_move_for_type(&m1_type, phys1);
    let move2 = signature_move_for_type(&m2_type, phys2);

    let calc_dmg = |off_s: i32, def_s: i32, eff: f32| -> i32 {
        if eff == 0.0 { return 0; }
        let base = ((22.0 * 90.0 * ((off_s * 2 + 36) as f32 / (def_s * 2 + 36).max(1) as f32)) / 50.0) + 2.0;
        (base * 1.5 * eff).round().max(1.0) as i32
    };
    let d12 = calc_dmg(if phys1 { get_stat(&s1, "Attack") } else { get_stat(&s1, "Sp. Atk") }, if phys1 { get_stat(&s2, "Defense") } else { get_stat(&s2, "Sp. Def") }, m1_mult);
    let d21 = calc_dmg(if phys2 { get_stat(&s2, "Attack") } else { get_stat(&s2, "Sp. Atk") }, if phys2 { get_stat(&s1, "Defense") } else { get_stat(&s1, "Sp. Def") }, m2_mult);

    println!("  {}", format!("├{}┤", "─".repeat(inner_w)).truecolor(gold.0, gold.1, gold.2));
    print_vs_wrapped(&format!("BST: {} ({}) vs {} ({}) | Wins: {}-{}", n1, bst1, n2, bst2, wins1, wins2), (255, 235, 140), true, content_w, gold);
    println!("  {}", format!("├{}┤", "─".repeat(inner_w)).truecolor(gold.0, gold.1, gold.2));

    let mut winner_name = "DRAW";
    for turn in 1..=3 {
        if hp1 <= 0 || hp2 <= 0 { break; }
        let mut log_hit = |atk: &str, def: &str, mv: &str, dmg: i32, eff: f32, dhp: &mut i32, dmax: i32, col: Rgb| {
            *dhp = (*dhp - dmg).max(0);
            let tag = if eff >= 3.9 { " [4x]" } else if eff >= 1.9 { " [2x]" } else if eff == 0.0 { " [0x]" } else if eff <= 0.51 { " [0.5x]" } else { "" };
            let txt = format!("T{}: {} used {} -> -{} HP{} ({}: {}/{})", turn, atk, mv, dmg, tag, def, *dhp, dmax);
            print_vs_wrapped(&txt, col, false, content_w, gold);
        };
        log_hit(n1, n2, move1, d12, m1_mult, &mut hp2, max_hp2, c1);
        if hp2 <= 0 { winner_name = n1; break; }
        log_hit(n2, n1, move2, d21, m2_mult, &mut hp1, max_hp1, c2);
        if hp1 <= 0 { winner_name = n2; break; }
    }

    let pct1 = ((hp1 as f32 / max_hp1 as f32) * 100.0).round() as i32;
    let pct2 = ((hp2 as f32 / max_hp2 as f32) * 100.0).round() as i32;
    if winner_name == "DRAW" {
        if pct1 > pct2 { winner_name = n1; } else if pct2 > pct1 { winner_name = n2; }
    }

    println!("  {}", format!("├{}┤", "─".repeat(inner_w)).truecolor(gold.0, gold.1, gold.2));
    if content_w >= 68 {
        let pad_end = content_w.saturating_sub(63);
        println!(
            "  {} {} {} {:>3}% │ {} {} {:>3}%{} {}",
            border_v,
            pad_right(n1, 10).truecolor(c1.0, c1.1, c1.2).bold(), hp_bar(hp1, max_hp1, 12), pct1,
            pad_right(n2, 10).truecolor(c2.0, c2.1, c2.2).bold(), hp_bar(hp2, max_hp2, 12), pct2,
            " ".repeat(pad_end),
            border_v
        );
    } else {
        let hp_w = content_w.saturating_sub(18).clamp(6, 24);
        let pad_end = content_w.saturating_sub(18 + hp_w);
        println!(
            "  {} {} {} {:>3}%{} {}",
            border_v,
            pad_right(n1, 10).truecolor(c1.0, c1.1, c1.2).bold(), hp_bar(hp1, max_hp1, hp_w), pct1,
            " ".repeat(pad_end), border_v
        );
        println!(
            "  {} {} {} {:>3}%{} {}",
            border_v,
            pad_right(n2, 10).truecolor(c2.0, c2.1, c2.2).bold(), hp_bar(hp2, max_hp2, hp_w), pct2,
            " ".repeat(pad_end), border_v
        );
    }

    let verdict = format!("WINNER: {} (Use --fight for playable battle)", winner_name);
    print_vs_wrapped(&verdict, gold, true, content_w, gold);
    println!("  {}\n", format!("╰{}╯", "─".repeat(inner_w)).truecolor(gold.0, gold.1, gold.2));

    if let Some(h) = audio_h { let _ = h.join(); }
}

pub fn run_quiz_mode(play_audio: bool) {
    let id = ((rand_seed() % 493) + 1).to_string();
    let Some(data) = fetch_pokemon(&id, false) else {
        eprintln!("  {}", "Failed to fetch a Pokemon for the quiz.".red());
        return;
    };
    let tw = term_width();
    let sp_max = (tw.saturating_sub(6) as u32).clamp(24, 56);
    let text_w = tw.saturating_sub(4).max(24);
    let name = data["name"].as_str().unwrap();
    let types: Vec<String> = data["types"].as_array().unwrap().iter().filter_map(|v| v.as_str().map(String::from)).collect();
    let theme = theme_for_types(&types);

    let audio_h = if play_audio {
        spawn_play_cry(data["id"].as_i64().unwrap_or(0), name.to_string(), data["cry_url"].as_str().unwrap_or("").to_string())
    } else { None };

    println!("\n  {}\n", "WHO'S THAT POKEMON?".truecolor(255, 220, 60).bold());
    if let Some(u) = data["normal_sprite"].as_str() {
        if let Some(bytes) = fetch_bytes(u) {
            let (_, sil) = render_sprite_lines(&bytes, sp_max, true);
            for line in sil { println!("  {}", line); }
        }
    }

    let hint = format!("HINT: {} | {} | {}", types.join("/"), data["generation"].as_str().unwrap_or(""), data["genus"].as_str().unwrap_or(""));
    println!();
    for line in wrap_text(&hint, text_w) {
        println!("  {}", line.cyan());
    }

    if let Some(clue) = data["quiz_clues"].as_array().and_then(|a| a.first()).and_then(|v| v.as_str()) {
        let mut redacted = clue.to_string();
        let mut hide = vec![name.to_string()];
        if let Some(evo) = data["evolution"].as_str() {
            for p in evo.split("->") { if !p.trim().is_empty() { hide.push(p.trim().to_string()); } }
        }
        for w in hide {
            if let Ok(re) = Regex::new(&format!("(?i){}", regex::escape(&w))) {
                redacted = re.replace_all(&redacted, "?????").to_string();
            }
        }
        let clue_txt = format!("CLUE: {}", redacted.chars().take(150).collect::<String>());
        for line in wrap_text(&clue_txt, text_w) {
            println!("  {}", line);
        }
    }

    print!("\n  Your Guess: ");
    let _ = io::stdout().flush();
    let mut guess = String::new();
    let _ = io::stdin().read_line(&mut guess);
    let g = guess.trim();

    let (dist, sim) = fuzzy_match(g, name);
    let pct = (sim * 100.0).round() as i32;
    let max_edits = if name.len() >= 8 { 3 } else { 2 };

    if dist == 0 {
        println!("\n  {}\n", format!("CORRECT! It's {}! (100% match)", name).green().bold());
    } else if g.len() >= 3 && sim >= 0.72 && dist <= max_edits {
        println!("\n  {}\n", format!("CLOSE ENOUGH! Accepted as {} ({}% match)", name, pct).truecolor(120, 240, 140).bold());
    } else {
        println!("\n  {}\n", format!("Wrong ({}% match) - it was {}!", pct, name).red().bold());
    }

    if let Some(u) = data["normal_sprite"].as_str() {
        if let Some(bytes) = fetch_bytes(u) {
            let (w, real) = render_sprite_lines(&bytes, sp_max, false);
            print_sprite_grid(&[(name.to_string(), w, real)], theme);
        }
    }
    if let Some(h) = audio_h { let _ = h.join(); }
}

pub fn catch_pokemon(data: &Value) {
    let path = box_path();
    let mut list: Vec<String> = fs::read_to_string(&path).ok().and_then(|c| serde_json::from_str(&c).ok()).unwrap_or_default();
    let entry = format!("#{:04} {}", data["id"].as_i64().unwrap_or(0), data["name"].as_str().unwrap_or(""));
    if !list.contains(&entry) {
        list.push(entry.clone());
        let _ = fs::write(path, serde_json::to_string_pretty(&list).unwrap_or_default());
        println!("  {}\n", format!("Gotcha! {} was added to your PC Box (total: {}).", entry, list.len()).green().bold());
    } else {
        println!("  {}\n", format!("{} is already in your PC Box.", entry).yellow());
    }
}

pub fn show_box() {
    let list: Vec<String> = fs::read_to_string(box_path()).ok().and_then(|c| serde_json::from_str(&c).ok()).unwrap_or_default();
    println!("\n  {}", format!("PC BOX ({} CAUGHT)", list.len()).bright_cyan().bold());
    if list.is_empty() {
        println!("  Box is empty. Catch one with: pokefact -p gengar --catch\n");
        return;
    }
    for (i, item) in list.iter().enumerate() {
        println!("  {:>2}. {}", i + 1, item.yellow());
    }
    println!();
}

pub fn trigger_missingno() {
    let tw = term_width().saturating_sub(4).max(28);
    let (glitch, cyan) = ((255, 50, 140), (60, 255, 220));
    println!("\n  {}", "▓▒░ DEX #000 — MISSINGNO. ░▒▓".truecolor(glitch.0, glitch.1, glitch.2).bold());
    for (i, row) in [
        "    ████  ██░░██  ", "    ██░░  ████░░  ", "    ░░██  ██  ██  ",
        "    ██████░░████  ", "    ██░░██  ████  ", "  ██████████████  ",
        "  ██░░████░░████  ", "  ████░░██  ████  ", "  ░░████████░░██  ",
    ].iter().enumerate() {
        let c = if i % 2 == 0 { glitch } else { cyan };
        println!("  {}", row.truecolor(c.0, c.1, c.2));
    }
    println!();
    for line in wrap_text("TYPE: Bird / Normal | HEIGHT: 3.0m | WEIGHT: 1590.8kg", tw) {
        println!("  {}", line.truecolor(cyan.0, cyan.1, cyan.2));
    }
    for line in wrap_text("ATTACKS: Water Gun, Water Gun, Sky Attack", tw) {
        println!("  {}", line.truecolor(glitch.0, glitch.1, glitch.2));
    }
    for line in wrap_text("Your 6th Bag item has been duplicated x128.", tw) {
        println!("  {}", line.yellow().bold());
    }
    println!();
}
