use std::collections::HashSet;
use std::fs;
use std::io::{self, Cursor, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use chrono::Local;
use colored::*;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use image::codecs::gif::GifDecoder;
use image::{imageops::FilterType, AnimationDecoder, DynamicImage, GenericImageView, Rgba};
use indicatif::{ProgressBar, ProgressStyle};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::modes::load_box_names;
use crate::{
    center_str, defensive_multipliers, fetch_bytes, fetch_pokemon, get_stat, hp_bar, pad_right,
    rand_seed, signature_move_for_type, spawn_play_cry, term_width, type_matchups, type_rgb,
    wrap_text, Capitalize, Rgb,
};

#[derive(Clone)]
pub struct BattleMove {
    pub name: String,
    pub mtype: String,
    pub power: i32,
    pub accuracy: i32,
    pub is_physical: bool,
    pub is_status: bool,
    pub ailment: String,
    pub ailment_chance: i32,
    pub pp: i32,
    pub max_pp: i32,
    pub priority: i32,
    pub drain: i32,
    pub healing: i32,
    pub stat_changes: Vec<(String, i32)>,
    pub target_self: bool,
    pub charge_turn: bool,
    pub recharge_turn: bool,
    pub desc: String,
    pub source: String,
}

#[derive(Clone, Copy, PartialEq)]
enum Status {
    None,
    Burn,
    Paralyze,
    Sleep(u8),
    Poison(u8),
    Freeze,
}

impl Status {
    fn tag(self) -> &'static str {
        match self {
            Status::None => "",
            Status::Burn => " [BRN]",
            Status::Paralyze => " [PAR]",
            Status::Sleep(_) => " [SLP]",
            Status::Poison(_) => " [PSN]",
            Status::Freeze => " [FRZ]",
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct BattleRecord {
    date: String,
    player: String,
    opponent: String,
    result: String,
    turns: u32,
    dmg: i32,
    hp_pct: i32,
    ranked: bool,
}

#[derive(Default, Serialize, Deserialize)]
struct StreakStore {
    current_streak: u32,
    best_streak: u32,
    total_wins: u32,
    total_losses: u32,
    current_chain: Vec<BattleRecord>,
    best_chain: Vec<BattleRecord>,
    history: Vec<BattleRecord>,
}

#[derive(Clone)]
struct Fighter {
    data: Value,
    id: i64,
    name: String,
    types: Vec<String>,
    ability: String,
    hp: i32,
    max_hp: i32,
    atk: i32,
    def: i32,
    spa: i32,
    spd: i32,
    spe: i32,
    stages: [i32; 5],
    status: Status,
    moves: Vec<BattleMove>,
    flash_fire_boost: bool,
    sitrus_berry: bool,
    charging_move: Option<usize>,
    must_recharge: bool,
    raw_frames: Vec<DynamicImage>,
    cached_key: (u32, usize),
    cached_w: usize,
    cached_frames: Vec<Vec<String>>,
}

impl Fighter {
    fn ensure_frames(&mut self, sp_w: u32, max_rows: usize) {
        let key = (sp_w, max_rows);
        if self.cached_key == key && !self.cached_frames.is_empty() {
            return;
        }
        self.cached_key = key;
        self.cached_frames.clear();

        if self.raw_frames.is_empty() {
            self.cached_w = 10;
            self.cached_frames.push(vec!["[NoSprite]".to_string()]);
            return;
        }

        let (cw, ch) = self.raw_frames[0].dimensions();
        let max_px_h = (max_rows.max(3) * 2) as u32;
        let scale = (sp_w as f32 / cw.max(1) as f32)
            .min(max_px_h as f32 / ch.max(1) as f32)
            .min(1.0);
        let tw = ((cw as f32 * scale).round() as u32).clamp(2, sp_w);
        let th = ((ch as f32 * scale).round() as u32).clamp(2, max_px_h);
        self.cached_w = tw as usize;

        for frame in &self.raw_frames {
            let resized = if tw != cw || th != ch {
                frame.resize_exact(tw, th, FilterType::Nearest)
            } else {
                frame.clone()
            };
            self.cached_frames.push(render_halfblock(&resized));
        }
    }
}

struct Bag {
    potions: u8,
    heals: u8,
    tonics: u8,
    balls: u8,
}

struct MatchStats {
    turns: u32,
    total_dmg: i32,
    super_hits: u32,
    crits: u32,
    caught: u32,
}

struct StagePose<'a> {
    fx_lines: &'a [String],
    fx_col: Rgb,
    msg: &'a str,
    sub_msg: &'a str,
    log_lines: &'a [String],
    menu_lines: &'a [String],
    hide_p: bool,
    hide_e: bool,
    p_shift: isize,
    e_shift: isize,
    p_drop: usize,
    e_drop: usize,
    p_hop: usize,
    e_hop: usize,
    anim_tick: usize,
    muted: bool,
    ranked: bool,
    streak: u32,
}

impl<'a> Default for StagePose<'a> {
    fn default() -> Self {
        Self {
            fx_lines: &[],
            fx_col: (255, 215, 70),
            msg: "",
            sub_msg: "",
            log_lines: &[],
            menu_lines: &[],
            hide_p: false,
            hide_e: false,
            p_shift: 0,
            e_shift: 0,
            p_drop: 0,
            e_drop: 0,
            p_hop: 0,
            e_hop: 0,
            anim_tick: 0,
            muted: false,
            ranked: false,
            streak: 0,
        }
    }
}

struct ScreenGuard;

impl Drop for ScreenGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let mut out = io::stdout();
        let _ = write!(out, "\x1b[?25h\x1b[?1049l");
        let _ = out.flush();
    }
}

// =========================================================================
// PROCEDURAL 8-BIT CHIPTUNE SYNTHESIZER & CROSS-PLATFORM BGM PLAYER
// =========================================================================

fn note_freq(midi: i32) -> f32 {
    if midi <= 0 {
        0.0
    } else {
        440.0 * 2.0f32.powf((midi - 69) as f32 / 12.0)
    }
}

fn encode_wav_pcm16(samples: &[i16], sample_rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for &s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

fn synth_battle_bgm_wav() -> Vec<u8> {
    let sr = 22050u32;
    let step_sec = 0.125f32; // 120 BPM 16th-note feel -> energetic battle drive
    let step_len = (sr as f32 * step_sec) as usize;

    // 64-step (8-second) heroic minor-key handheld RPG battle loop
    let lead: [i32; 64] = [
        69, 0, 72, 76, 74, 0, 72, 71, 69, 71, 72, 74, 76, 0, 76, 0,
        77, 0, 76, 74, 72, 0, 71, 69, 68, 0, 71, 74, 76, 0, 0, 0,
        81, 0, 79, 76, 77, 0, 76, 74, 72, 74, 76, 79, 81, 0, 76, 0,
        74, 76, 77, 74, 72, 74, 76, 72, 71, 74, 76, 79, 81, 0, 76, 0,
    ];
    let bass: [i32; 64] = [
        45, 57, 45, 57, 45, 57, 45, 57, 48, 60, 48, 60, 48, 60, 48, 60,
        41, 53, 41, 53, 43, 55, 43, 55, 40, 52, 40, 52, 40, 52, 44, 56,
        45, 57, 45, 57, 41, 53, 41, 53, 48, 60, 48, 60, 43, 55, 43, 55,
        50, 62, 50, 62, 48, 60, 48, 60, 40, 52, 44, 56, 45, 57, 45, 52,
    ];

    let mut samples = Vec::with_capacity(lead.len() * step_len);
    let mut rng = 0x1234_5678u32;

    for step in 0..lead.len() {
        let lf = note_freq(lead[step]);
        let bf = note_freq(bass[step]);
        let has_hit = step % 4 == 0 || step % 4 == 2;
        let is_snare = step % 4 == 2;

        for i in 0..step_len {
            let t = i as f32 / sr as f32;
            let env = (1.0 - (i as f32 / step_len as f32) * 0.45).max(0.15);

            // Pulse-wave lead (25% duty cycle)
            let l_sig = if lf > 0.0 {
                let phase = (t * lf) % 1.0;
                (if phase < 0.25 { 1.0 } else { -1.0 }) * 0.24 * env
            } else {
                0.0
            };

            // Triangle-wave bass
            let b_sig = if bf > 0.0 {
                let p = (t * bf) % 1.0;
                let tri = if p < 0.5 { 4.0 * p - 1.0 } else { 3.0 - 4.0 * p };
                tri * 0.28 * env
            } else {
                0.0
            };

            // Crisp retro noise percussion
            let p_sig = if has_hit && i < (if is_snare { 900 } else { 420 }) {
                rng = rng.wrapping_mul(1664525).wrapping_add(1013904223);
                let n = ((rng >> 16) as f32 / 32768.0) - 1.0;
                let decay = 1.0 - (i as f32 / if is_snare { 900.0 } else { 420.0 });
                n * (if is_snare { 0.16 } else { 0.10 }) * decay
            } else {
                0.0
            };

            let mix = (l_sig + b_sig + p_sig).clamp(-0.95, 0.95);
            samples.push((mix * 15000.0) as i16);
        }
    }
    encode_wav_pcm16(&samples, sr)
}

fn synth_victory_fanfare_wav() -> Vec<u8> {
    let sr = 22050u32;
    let notes: &[(i32, f32)] = &[
        (72, 0.11), (76, 0.11), (79, 0.11), (84, 0.28),
        (79, 0.12), (84, 0.55),
    ];
    let mut samples = Vec::new();
    for &(midi, dur) in notes {
        let len = (sr as f32 * dur) as usize;
        let f1 = note_freq(midi);
        let f2 = note_freq(midi - 12);
        for i in 0..len {
            let t = i as f32 / sr as f32;
            let env = (1.0 - (i as f32 / len as f32) * 0.6).max(0.1);
            let sq = if (t * f1) % 1.0 < 0.3 { 1.0 } else { -1.0 };
            let harm = if (t * f2) % 1.0 < 0.5 { 0.6 } else { -0.6 };
            let mix = (sq * 0.32 + harm * 0.22) * env;
            samples.push((mix * 16000.0) as i16);
        }
    }
    encode_wav_pcm16(&samples, sr)
}

struct BgmPlayer {
    running: Arc<AtomicBool>,
    muted: Arc<AtomicBool>,
    #[cfg(target_os = "android")]
    child: Arc<std::sync::Mutex<Option<std::process::Child>>>,
}

impl BgmPlayer {
    fn start(initially_muted: bool) -> Self {
        let running = Arc::new(AtomicBool::new(true));
        let muted = Arc::new(AtomicBool::new(initially_muted));
        let wav = synth_battle_bgm_wav();

        #[cfg(target_os = "android")]
        let child_slot: Arc<std::sync::Mutex<Option<std::process::Child>>> =
            Arc::new(std::sync::Mutex::new(None));

        #[cfg(target_os = "android")]
        {
            let run_c = Arc::clone(&running);
            let mute_c = Arc::clone(&muted);
            let child_c = Arc::clone(&child_slot);
            thread::spawn(move || {
                use std::process::{Command, Stdio};
                let mut dir = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
                dir.push(".pokefact_cries");
                let _ = fs::create_dir_all(&dir);
                let path = dir.join("pokefact_battle_bgm.wav");
                if fs::write(&path, &wav).is_err() {
                    return;
                }
                let p = path.to_string_lossy().to_string();

                let bin_opt = if Command::new("play-audio").arg("--help").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok() {
                    Some(("play-audio", vec![p.clone()]))
                } else if Command::new("mpv").arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok() {
                    Some(("mpv", vec!["--no-video".into(), "--really-quiet".into(), p.clone()]))
                } else {
                    None
                };
                let Some((bin, args)) = bin_opt else { return; };

                while run_c.load(Ordering::SeqCst) {
                    if mute_c.load(Ordering::SeqCst) {
                        thread::sleep(Duration::from_millis(120));
                        continue;
                    }
                    let Ok(ch) = Command::new(bin)
                        .args(&args)
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .spawn()
                    else {
                        return;
                    };
                    if let Ok(mut guard) = child_c.lock() {
                        *guard = Some(ch);
                    }
                    loop {
                        if !run_c.load(Ordering::SeqCst) || mute_c.load(Ordering::SeqCst) {
                            if let Ok(mut guard) = child_c.lock() {
                                if let Some(ref mut c) = *guard {
                                    let _ = c.kill();
                                    let _ = c.wait();
                                }
                                *guard = None;
                            }
                            break;
                        }
                        let done = if let Ok(mut guard) = child_c.lock() {
                            if let Some(ref mut c) = *guard {
                                matches!(c.try_wait(), Ok(Some(_)) | Err(_))
                            } else {
                                true
                            }
                        } else {
                            true
                        };
                        if done {
                            if let Ok(mut guard) = child_c.lock() {
                                *guard = None;
                            }
                            break;
                        }
                        thread::sleep(Duration::from_millis(80));
                    }
                }
            });
        }

        #[cfg(not(target_os = "android"))]
        {
            let run_c = Arc::clone(&running);
            let mute_c = Arc::clone(&muted);
            thread::spawn(move || {
                use rodio::{Decoder, OutputStream, Sink, Source};
                let Ok((_stream, handle)) = OutputStream::try_default() else { return; };
                let Ok(sink) = Sink::try_new(&handle) else { return; };
                let Ok(source) = Decoder::new(Cursor::new(wav)) else { return; };
                sink.set_volume(0.35);
                sink.append(source.repeat_infinite());
                if mute_c.load(Ordering::SeqCst) {
                    sink.pause();
                }
                let mut was_muted = mute_c.load(Ordering::SeqCst);
                while run_c.load(Ordering::SeqCst) {
                    let cur_m = mute_c.load(Ordering::SeqCst);
                    if cur_m != was_muted {
                        if cur_m { sink.pause(); } else { sink.play(); }
                        was_muted = cur_m;
                    }
                    thread::sleep(Duration::from_millis(60));
                }
                sink.stop();
            });
        }

        Self {
            running,
            muted,
            #[cfg(target_os = "android")]
            child: child_slot,
        }
    }

    fn toggle_mute(&self) -> bool {
        let next = !self.muted.load(Ordering::SeqCst);
        self.muted.store(next, Ordering::SeqCst);
        next
    }

    fn is_muted(&self) -> bool {
        self.muted.load(Ordering::SeqCst)
    }

    fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
        #[cfg(target_os = "android")]
        if let Ok(mut guard) = self.child.lock() {
            if let Some(ref mut c) = *guard {
                let _ = c.kill();
                let _ = c.wait();
            }
            *guard = None;
        }
    }
}

impl Drop for BgmPlayer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn play_victory_fanfare(muted: bool) {
    if muted {
        return;
    }
    let wav = synth_victory_fanfare_wav();
    thread::spawn(move || {
        #[cfg(target_os = "android")]
        {
            use std::process::{Command, Stdio};
            let mut dir = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
            dir.push(".pokefact_cries");
            let _ = fs::create_dir_all(&dir);
            let path = dir.join("pokefact_victory_bgm.wav");
            if fs::write(&path, &wav).is_ok() {
                let p = path.to_string_lossy().to_string();
                let _ = Command::new("play-audio").arg(&p).stdout(Stdio::null()).stderr(Stdio::null()).status()
                    .or_else(|_| Command::new("mpv").args(["--no-video", "--really-quiet", &p]).stdout(Stdio::null()).stderr(Stdio::null()).status());
            }
        }
        #[cfg(not(target_os = "android"))]
        {
            use rodio::{Decoder, OutputStream, Sink};
            if let Ok((_stream, handle)) = OutputStream::try_default() {
                if let Ok(sink) = Sink::try_new(&handle) {
                    if let Ok(source) = Decoder::new(Cursor::new(wav)) {
                        sink.set_volume(0.45);
                        sink.append(source);
                        sink.sleep_until_end();
                    }
                }
            }
        }
    });
}

// =========================================================================
// PERSISTENT STREAKS, HISTORY & QUIET PC BOX SAVE
// =========================================================================

fn streak_path() -> PathBuf {
    let mut dir = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    dir.push(".pokefact_streaks.json");
    dir
}

fn load_streaks() -> StreakStore {
    fs::read_to_string(streak_path())
        .ok()
        .and_then(|c| serde_json::from_str(&c).ok())
        .unwrap_or_default()
}

fn save_streaks(store: &StreakStore) {
    let _ = fs::write(
        streak_path(),
        serde_json::to_string_pretty(store).unwrap_or_default(),
    );
}

fn record_battle_outcome(rec: BattleRecord) -> (u32, u32) {
    let mut store = load_streaks();
    let is_win = rec.result == "WIN" || rec.result == "CAUGHT";

    if rec.ranked {
        if is_win {
            store.current_streak += 1;
            store.total_wins += 1;
            store.current_chain.push(rec.clone());
            if store.current_streak > store.best_streak {
                store.best_streak = store.current_streak;
                store.best_chain = store.current_chain.clone();
            }
        } else {
            store.current_streak = 0;
            store.total_losses += 1;
            store.current_chain.clear();
        }
    }

    store.history.push(rec);
    if store.history.len() > 100 {
        let excess = store.history.len() - 100;
        store.history.drain(0..excess);
    }
    save_streaks(&store);
    (store.current_streak, store.best_streak)
}

fn save_to_box_quiet(data: &Value) {
    let mut path = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push(".pokefact_box.json");
    let mut list: Vec<String> = fs::read_to_string(&path)
        .ok()
        .and_then(|c| serde_json::from_str(&c).ok())
        .unwrap_or_default();
    let entry = format!(
        "#{:04} {}",
        data["id"].as_i64().unwrap_or(0),
        data["name"].as_str().unwrap_or("")
    );
    if !list.contains(&entry) {
        list.push(entry);
        let _ = fs::write(path, serde_json::to_string_pretty(&list).unwrap_or_default());
    }
}

pub fn show_streak() {
    let store = load_streaks();
    let tw = term_width();
    let inner = tw.saturating_sub(5).clamp(32, 86);
    let cw = inner - 2;
    let gold = (255, 215, 70);
    let bv = "│".truecolor(gold.0, gold.1, gold.2);

    let total = store.total_wins + store.total_losses;
    let wr = if total > 0 {
        (store.total_wins as f32 / total as f32) * 100.0
    } else {
        0.0
    };

    println!();
    println!("  {}", format!("╭{}╮", "─".repeat(inner)).truecolor(gold.0, gold.1, gold.2));
    println!(
        "  {} {} {}",
        bv,
        center_str("POKEFACT RANKED BATTLE STREAK CARD", cw).truecolor(gold.0, gold.1, gold.2).bold(),
        bv
    );
    println!("  {}", format!("├{}┤", "─".repeat(inner)).truecolor(gold.0, gold.1, gold.2));
    println!(
        "  {} {} {}",
        bv,
        pad_right(&format!("CURRENT STREAK : {} WIN(S)", store.current_streak), cw)
            .truecolor(90, 235, 115)
            .bold(),
        bv
    );
    println!(
        "  {} {} {}",
        bv,
        pad_right(&format!("HIGHEST STREAK : {} WIN(S)", store.best_streak), cw)
            .truecolor(80, 240, 255)
            .bold(),
        bv
    );
    println!(
        "  {} {} {}",
        bv,
        pad_right(
            &format!(
                "RANKED RECORD  : {}W - {}L ({:.1}% WR)",
                store.total_wins, store.total_losses, wr
            ),
            cw
        )
        .truecolor(230, 235, 245),
        bv
    );

    if !store.current_chain.is_empty() {
        println!("  {}", format!("├{}┤", "─".repeat(inner)).truecolor(gold.0, gold.1, gold.2));
        println!(
            "  {} {} {}",
            bv,
            pad_right("ACTIVE STREAK MATCHUPS:", cw).truecolor(90, 235, 115).bold(),
            bv
        );
        for (i, r) in store.current_chain.iter().enumerate() {
            let line = format!("#{} {} def. {} ({}t, {}% HP)", i + 1, r.player, r.opponent, r.turns, r.hp_pct);
            for w in wrap_text(&line, cw) {
                println!("  {} {} {}", bv, pad_right(&w, cw).truecolor(220, 230, 240), bv);
            }
        }
    }

    if !store.best_chain.is_empty() {
        println!("  {}", format!("├{}┤", "─".repeat(inner)).truecolor(gold.0, gold.1, gold.2));
        println!(
            "  {} {} {}",
            bv,
            pad_right("RECORD STREAK HALL OF FAME:", cw).truecolor(80, 240, 255).bold(),
            bv
        );
        for (i, r) in store.best_chain.iter().enumerate() {
            let line = format!("#{} {} def. {} ({}t, {}% HP)", i + 1, r.player, r.opponent, r.turns, r.hp_pct);
            for w in wrap_text(&line, cw) {
                println!("  {} {} {}", bv, pad_right(&w, cw).truecolor(210, 225, 240), bv);
            }
        }
    } else {
        println!("  {}", format!("├{}┤", "─".repeat(inner)).truecolor(gold.0, gold.1, gold.2));
        for w in wrap_text("Start a Ranked Random Battle with: pokefact --battle <your_pokemon>", cw) {
            println!("  {} {} {}", bv, pad_right(&w, cw).dimmed(), bv);
        }
    }

    println!("  {}\n", format!("╰{}╯", "─".repeat(inner)).truecolor(gold.0, gold.1, gold.2));
}

pub fn show_history() {
    let store = load_streaks();
    let tw = term_width();
    let inner = tw.saturating_sub(5).clamp(32, 88);
    let cw = inner - 2;
    let cyan = (110, 220, 255);
    let bv = "│".truecolor(cyan.0, cyan.1, cyan.2);

    let ranked_hist: Vec<&BattleRecord> = store.history.iter().rev().filter(|r| r.ranked).take(25).collect();

    println!();
    println!("  {}", format!("╭{}╮", "─".repeat(inner)).truecolor(cyan.0, cyan.1, cyan.2));
    println!(
        "  {} {} {}",
        bv,
        center_str(&format!("RANDOM OPPONENT BATTLE HISTORY ({})", ranked_hist.len()), cw)
            .truecolor(cyan.0, cyan.1, cyan.2)
            .bold(),
        bv
    );
    println!("  {}", format!("├{}┤", "─".repeat(inner)).truecolor(cyan.0, cyan.1, cyan.2));

    if ranked_hist.is_empty() {
        for w in wrap_text("No random opponent battles yet. Run: pokefact --battle gengar", cw) {
            println!("  {} {} {}", bv, pad_right(&w, cw).dimmed(), bv);
        }
    } else {
        for (i, r) in ranked_hist.iter().enumerate() {
            let col = if r.result == "WIN" || r.result == "CAUGHT" {
                (90, 235, 115)
            } else {
                (255, 95, 95)
            };
            let txt = format!(
                "{:>2}. [{}] {} vs {} | {}t {}dmg | {}",
                i + 1, r.result, r.player, r.opponent, r.turns, r.dmg, r.date
            );
            for w in wrap_text(&txt, cw) {
                println!("  {} {} {}", bv, pad_right(&w, cw).truecolor(col.0, col.1, col.2), bv);
            }
        }
    }

    println!("  {}\n", format!("╰{}╯", "─".repeat(inner)).truecolor(cyan.0, cyan.1, cyan.2));
}

// =========================================================================
// SPRITE DECODING (ANIMATED GIF + STATIC FALLBACK)
// =========================================================================

fn render_halfblock(img: &DynamicImage) -> Vec<String> {
    let (fw, fh) = img.dimensions();
    let mut rows = Vec::new();
    for y in (0..fh).step_by(2) {
        let mut line = String::new();
        for x in 0..fw {
            let top = img.get_pixel(x, y);
            let bot = if y + 1 < fh {
                img.get_pixel(x, y + 1)
            } else {
                Rgba([0, 0, 0, 0])
            };
            match (top[3] > 25, bot[3] > 25) {
                (false, false) => line.push(' '),
                (true, false) => line.push_str(&format!("\x1b[38;2;{};{};{}m▀\x1b[0m", top[0], top[1], top[2])),
                (false, true) => line.push_str(&format!("\x1b[38;2;{};{};{}m▄\x1b[0m", bot[0], bot[1], bot[2])),
                (true, true) => line.push_str(&format!(
                    "\x1b[38;2;{};{};{};48;2;{};{};{}m▀\x1b[0m",
                    top[0], top[1], top[2], bot[0], bot[1], bot[2]
                )),
            }
        }
        rows.push(line);
    }
    rows
}

fn decode_animated_or_static(bytes: &[u8]) -> Vec<DynamicImage> {
    if let Ok(dec) = GifDecoder::new(Cursor::new(bytes)) {
        if let Ok(frames) = dec.into_frames().collect_frames() {
            if !frames.is_empty() {
                let total = frames.len();
                let step = (total / 12).max(1);
                let mut raw = Vec::new();
                let (mut min_x, mut min_y, mut max_x, mut max_y) = (u32::MAX, u32::MAX, 0u32, 0u32);
                for (i, f) in frames.into_iter().enumerate() {
                    if i % step != 0 && raw.len() >= 12 {
                        continue;
                    }
                    let img = DynamicImage::ImageRgba8(f.into_buffer());
                    for (x, y, p) in img.pixels() {
                        if p[3] > 20 {
                            min_x = min_x.min(x);
                            min_y = min_y.min(y);
                            max_x = max_x.max(x);
                            max_y = max_y.max(y);
                        }
                    }
                    raw.push(img);
                    if raw.len() >= 12 {
                        break;
                    }
                }
                if !raw.is_empty() {
                    let (cx, cy, cw, ch) = if max_x >= min_x && max_y >= min_y {
                        (min_x, min_y, (max_x - min_x) + 1, (max_y - min_y) + 1)
                    } else {
                        let (w, h) = raw[0].dimensions();
                        (0, 0, w.max(1), h.max(1))
                    };
                    return raw.into_iter().map(|im| im.crop_imm(cx, cy, cw, ch)).collect();
                }
            }
        }
    }

    if let Ok(img) = image::load_from_memory(bytes) {
        let (w, h) = img.dimensions();
        let (mut min_x, mut min_y, mut max_x, mut max_y) = (w, h, 0, 0);
        for (x, y, p) in img.pixels() {
            if p[3] > 20 {
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
        }
        let cropped = if max_x >= min_x && max_y >= min_y {
            img.crop_imm(min_x, min_y, (max_x - min_x) + 1, (max_y - min_y) + 1)
        } else {
            img
        };
        return vec![cropped];
    }
    vec![]
}

// =========================================================================
// MOVES, STATS, FAIR EQUALIZER & GRANDMASTER AI
// =========================================================================

fn term_dims() -> (usize, usize) {
    terminal_size::terminal_size()
        .map(|(terminal_size::Width(w), terminal_size::Height(h))| (w as usize, h as usize))
        .unwrap_or((80, 26))
}

fn fair_scale_factor(bst: i32) -> f32 {
    if bst < 500 {
        (500.0 / (bst.max(180) as f32)).powf(0.62).min(1.75)
    } else if bst > 600 {
        (600.0 / (bst as f32)).powf(0.45)
    } else {
        1.0
    }
}

fn stage_mult(stage: i32) -> f32 {
    let s = stage.clamp(-6, 6);
    if s >= 0 {
        (2.0 + s as f32) / 2.0
    } else {
        2.0 / (2.0 - s as f32)
    }
}

pub fn fetch_move_detail(move_slug: &str) -> Option<BattleMove> {
    fetch_move_with_source(move_slug, "GAME")
}

fn fetch_move_with_source(move_slug: &str, source: &str) -> Option<BattleMove> {
    let slug = move_slug
        .trim()
        .to_lowercase()
        .replace(' ', "-")
        .replace('\'', "");
    let url = format!("https://pokeapi.co/api/v2/move/{}", slug);
    let json: Value = ureq::get(&url).call().ok()?.into_json().ok()?;
    let name = json["name"]
        .as_str()?
        .split('-')
        .map(|s| s.capitalize())
        .collect::<Vec<_>>()
        .join(" ");
    let mtype = json["type"]["name"].as_str()?.capitalize();
    let dclass = json["damage_class"]["name"].as_str().unwrap_or("status");
    let power = json["power"].as_i64().unwrap_or(0) as i32;
    let accuracy = json["accuracy"].as_i64().unwrap_or(100) as i32;
    let pp = json["pp"].as_i64().unwrap_or(15).clamp(5, 40) as i32;
    let priority = json["priority"].as_i64().unwrap_or(0) as i32;
    let ailment = json["meta"]["ailment"]["name"].as_str().unwrap_or("none").to_string();
    let ailment_chance = json["meta"]["ailment_chance"].as_i64().unwrap_or(0) as i32;
    let drain = json["meta"]["drain"].as_i64().unwrap_or(0) as i32;
    let healing = json["meta"]["healing"].as_i64().unwrap_or(0) as i32;
    let target_name = json["target"]["name"].as_str().unwrap_or("selected-pokemon");
    let target_self = matches!(target_name, "user" | "users-field" | "user-and-allies");

    let mut stat_changes = Vec::new();
    if let Some(arr) = json["stat_changes"].as_array() {
        for sc in arr {
            if let (Some(sn), Some(ch)) = (sc["stat"]["name"].as_str(), sc["change"].as_i64()) {
                stat_changes.push((sn.to_string(), ch as i32));
            }
        }
    }

    let eff_chance = json["effect_chance"].as_i64().unwrap_or(ailment_chance as i64);
    let mut desc = json["effect_entries"]
        .as_array()
        .and_then(|arr| arr.iter().find(|e| e["language"]["name"].as_str() == Some("en")))
        .and_then(|e| e["short_effect"].as_str())
        .map(|s| s.replace("$effect_chance", &eff_chance.to_string()))
        .or_else(|| {
            json["flavor_text_entries"]
                .as_array()
                .and_then(|arr| arr.iter().find(|e| e["language"]["name"].as_str() == Some("en")))
                .and_then(|e| e["flavor_text"].as_str())
                .map(|s| s.replace(['\n', '\r', '\x0c'], " "))
        })
        .unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");

    let desc_low = desc.to_lowercase();
    let charge_turn = desc_low.contains("turn to charge") || desc_low.contains("charges on turn");
    let recharge_turn = desc_low.contains("must recharge") || desc_low.contains("forgo its next turn");

    if desc.is_empty() {
        desc = if dclass == "status" {
            format!("{} support move.", mtype)
        } else {
            format!(
                "{} {} attack ({} power).",
                if dclass == "physical" { "Physical" } else { "Special" },
                mtype,
                power
            )
        };
    }

    Some(BattleMove {
        name,
        mtype,
        power,
        accuracy,
        is_physical: dclass == "physical",
        is_status: dclass == "status",
        ailment,
        ailment_chance,
        pp,
        max_pp: pp,
        priority,
        drain,
        healing,
        stat_changes,
        target_self,
        charge_turn,
        recharge_turn,
        desc,
        source: source.to_string(),
    })
}

fn build_canon_movepool(data: &Value, types: &[String], phys: bool, is_opponent: bool) -> Vec<BattleMove> {
    let mut seen = HashSet::new();
    let mut candidates: Vec<(String, &'static str)> = Vec::new();

    let mut push_cand = |raw: &str, src: &'static str| {
        let slug = raw.trim().to_lowercase().replace(' ', "-").replace('\'', "");
        if !slug.is_empty() && seen.insert(slug.clone()) {
            candidates.push((slug, src));
        }
    };

    if let Some(arr) = data["anime_moves"].as_array() {
        for m in arr.iter().take(5) {
            if let Some(n) = m.as_str() { push_cand(n, "ANIME"); }
        }
    }
    if let Some(arr) = data["manga_moves"].as_array() {
        for m in arr.iter().take(4) {
            if let Some(n) = m.as_str() { push_cand(n, "MANGA"); }
        }
    }
    if let Some(arr) = data["level_moves"].as_array() {
        for m in arr.iter().rev().take(10) {
            if let Some(n) = m[1].as_str() { push_cand(n, "GAME"); }
        }
    }
    if let Some(arr) = data["tm_moves"].as_array() {
        for m in arr.iter().take(6) {
            if let Some(n) = m.as_str() { push_cand(n, "TM"); }
        }
    }

    let fetched: Vec<BattleMove> = thread::scope(|s| {
        let handles: Vec<_> = candidates
            .into_iter()
            .take(16)
            .map(|(slug, src)| s.spawn(move || fetch_move_with_source(&slug, src)))
            .collect();
        handles.into_iter().filter_map(|h| h.join().ok().flatten()).collect()
    });

    let mut dmg: Vec<BattleMove> = fetched
        .iter()
        .filter(|m| !m.is_status && m.power > 0 && (!is_opponent || !m.charge_turn))
        .cloned()
        .collect();
    let mut sup: Vec<BattleMove> = fetched
        .iter()
        .filter(|m| {
            m.is_status
                && (!m.stat_changes.is_empty()
                    || m.healing > 0
                    || matches!(m.ailment.as_str(), "burn" | "paralysis" | "sleep" | "poison" | "freeze"))
        })
        .cloned()
        .collect();

    // Sort damaging moves by effective competitive value (STAB + power * accuracy)
    dmg.sort_by(|a, b| {
        let score = |m: &BattleMove| -> i32 {
            let stab = if types.iter().any(|t| t.eq_ignore_ascii_case(&m.mtype)) { 35 } else { 0 };
            let match_cat = if m.is_physical == phys { 20 } else { 0 };
            (m.power * m.accuracy / 100) + stab + match_cat + (m.priority * 15)
        };
        score(b).cmp(&score(a))
    });

    let mut deck: Vec<BattleMove> = dmg.into_iter().take(8).collect();

    // Ensure every fighter has at least one reliable STAB signature move
    let primary_type = types.first().cloned().unwrap_or_else(|| "Normal".to_string());
    let has_reliable_stab = deck.iter().any(|m| {
        !m.is_status && !m.charge_turn && m.power >= 65 && m.mtype.eq_ignore_ascii_case(&primary_type)
    });
    if !has_reliable_stab {
        deck.insert(0, BattleMove {
            name: signature_move_for_type(&primary_type, phys).to_string(),
            mtype: primary_type.clone(),
            power: 85,
            accuracy: 95,
            is_physical: phys,
            is_status: false,
            ailment: "none".to_string(),
            ailment_chance: 0,
            pp: 15,
            max_pp: 15,
            priority: 0,
            drain: 0,
            healing: 0,
            stat_changes: vec![],
            target_self: false,
            charge_turn: false,
            recharge_turn: false,
            desc: format!("Signature {} attack mastered through battle training.", primary_type),
            source: "CANON".to_string(),
        });
    }

    deck.extend(sup.drain(..sup.len().min(4)));

    if !deck.iter().any(|m| m.is_status) {
        deck.push(BattleMove {
            name: "Focus Spirit".to_string(),
            mtype: primary_type,
            power: 0,
            accuracy: 100,
            is_physical: false,
            is_status: true,
            ailment: "none".to_string(),
            ailment_chance: 0,
            pp: 10,
            max_pp: 10,
            priority: 0,
            drain: 0,
            healing: 0,
            stat_changes: vec![("attack".into(), 1), ("special-attack".into(), 1)],
            target_self: true,
            charge_turn: false,
            recharge_turn: false,
            desc: "Channels battle spirit to raise Attack and Sp. Atk by +1 stage.".to_string(),
            source: "ANIME".to_string(),
        });
    }

    deck
}

fn build_fighter(data: Value, is_player: bool) -> Fighter {
    let id = data["id"].as_i64().unwrap_or(0);
    let name = data["name"].as_str().unwrap_or("UNKNOWN").to_string();
    let slug: String = name.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect();
    let types: Vec<String> = data["types"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();
    let ability = data["abilities"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(|v| v.as_str())
        .unwrap_or("Inner Focus")
        .replace(" (Hidden)", "");

    let stats: Vec<(String, i32)> = data["stats"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|s| Some((s[0].as_str()?.to_string(), s[1].as_i64()? as i32)))
        .collect();

    let bst: i32 = stats.iter().map(|(_, v)| *v).sum();
    let sc = fair_scale_factor(bst);
    // Opponent gets competitive EV/Nature training (+12%) for a genuinely hard boss fight
    let opp_boost = if is_player { 1.0 } else { 1.12 };
    let scaled = |k: &str| -> i32 {
        ((get_stat(&stats, k) as f32) * sc * opp_boost).round().max(28.0) as i32
    };

    let hp_base = scaled("HP");
    let atk_base = scaled("Attack");
    let def_base = scaled("Defense");
    let spa_base = scaled("Sp. Atk");
    let spd_base = scaled("Sp. Def");
    let spe_base = scaled("Speed");

    let max_hp = hp_base * 2 + 115;
    let phys = atk_base >= spa_base;

    let front_png = data["normal_sprite"].as_str().unwrap_or("").to_string();
    let back_png = format!("https://raw.githubusercontent.com/PokeAPI/sprites/master/sprites/pokemon/back/{}.png", id);
    let api_ani_front = data["animated_sprite"].as_str().unwrap_or("").to_string();
    let sd_gen5_back = format!("https://play.pokemonshowdown.com/sprites/gen5ani-back/{}.gif", slug);
    let sd_ani_back = format!("https://play.pokemonshowdown.com/sprites/ani-back/{}.gif", slug);
    let sd_gen5_front = format!("https://play.pokemonshowdown.com/sprites/gen5ani/{}.gif", slug);
    let sd_ani_front = format!("https://play.pokemonshowdown.com/sprites/ani/{}.gif", slug);

    let (moves, raw_frames) = thread::scope(|s| {
        let hm = s.spawn(|| build_canon_movepool(&data, &types, phys, !is_player));
        let hs = s.spawn(|| {
            let bytes_opt = if is_player {
                fetch_bytes(&sd_gen5_back)
                    .or_else(|| fetch_bytes(&sd_ani_back))
                    .or_else(|| fetch_bytes(&back_png))
                    .or_else(|| fetch_bytes(&front_png))
            } else {
                fetch_bytes(&api_ani_front)
                    .or_else(|| fetch_bytes(&sd_gen5_front))
                    .or_else(|| fetch_bytes(&sd_ani_front))
                    .or_else(|| fetch_bytes(&front_png))
            };
            bytes_opt.map(|b| decode_animated_or_static(&b)).unwrap_or_default()
        });
        (hm.join().unwrap_or_default(), hs.join().unwrap_or_default())
    });

    Fighter {
        data,
        id,
        name,
        types,
        ability,
        hp: max_hp,
        max_hp,
        atk: atk_base * 2 + 36,
        def: def_base * 2 + 36,
        spa: spa_base * 2 + 36,
        spd: spd_base * 2 + 36,
        spe: spe_base * 2 + 36,
        stages: [0; 5],
        status: Status::None,
        moves,
        flash_fire_boost: false,
        sitrus_berry: !is_player,
        charging_move: None,
        must_recharge: false,
        raw_frames,
        cached_key: (0, 0),
        cached_w: 10,
        cached_frames: vec![],
    }
}

fn ability_is(f: &Fighter, key: &str) -> bool {
    f.ability.to_lowercase().contains(key)
}

fn effective_speed(f: &Fighter) -> i32 {
    let mut s = (f.spe as f32) * stage_mult(f.stages[4]);
    if f.status == Status::Paralyze {
        s *= 0.5;
    }
    s.round() as i32
}

fn calc_move_damage(mv: &BattleMove, atk: &Fighter, def: &Fighter, crit: bool, roll: f32) -> (i32, f32, Option<&'static str>) {
    if mv.is_status || mv.power <= 0 {
        return (0, 1.0, None);
    }
    if mv.mtype.eq_ignore_ascii_case("Ground") && ability_is(def, "levitate") {
        return (0, 0.0, Some("Levitate"));
    }
    if mv.mtype.eq_ignore_ascii_case("Fire") && ability_is(def, "flash fire") {
        return (0, 0.0, Some("Flash Fire"));
    }
    if mv.mtype.eq_ignore_ascii_case("Water") && ability_is(def, "water absorb") {
        return (0, 0.0, Some("Water Absorb"));
    }
    if mv.mtype.eq_ignore_ascii_case("Electric") && ability_is(def, "volt absorb") {
        return (0, 0.0, Some("Volt Absorb"));
    }

    let mults = defensive_multipliers(&def.types);
    let eff = mults
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(&mv.mtype))
        .map(|(_, &m)| m)
        .unwrap_or(1.0);
    if eff == 0.0 {
        return (0, 0.0, None);
    }

    let mut pow = mv.power as f32;
    if ability_is(atk, "technician") && mv.power <= 60 {
        pow *= 1.5;
    }
    if atk.flash_fire_boost && mv.mtype.eq_ignore_ascii_case("Fire") {
        pow *= 1.5;
    }
    let low_hp = atk.hp * 3 <= atk.max_hp;
    if low_hp
        && ((ability_is(atk, "blaze") && mv.mtype.eq_ignore_ascii_case("Fire"))
            || (ability_is(atk, "torrent") && mv.mtype.eq_ignore_ascii_case("Water"))
            || (ability_is(atk, "overgrow") && mv.mtype.eq_ignore_ascii_case("Grass"))
            || (ability_is(atk, "swarm") && mv.mtype.eq_ignore_ascii_case("Bug")))
    {
        pow *= 1.5;
    }

    let atk_stage = if mv.is_physical { atk.stages[0] } else { atk.stages[2] };
    let def_stage = if mv.is_physical { def.stages[1] } else { def.stages[3] };
    let mut off_val = (if mv.is_physical { atk.atk } else { atk.spa } as f32)
        * stage_mult(if crit && atk_stage < 0 { 0 } else { atk_stage });
    if mv.is_physical && atk.status == Status::Burn {
        off_val *= 0.5;
    }
    let def_val = ((if mv.is_physical { def.def } else { def.spd } as f32)
        * stage_mult(if crit && def_stage > 0 { 0 } else { def_stage }))
    .max(1.0);

    let has_stab = atk.types.iter().any(|t| t.eq_ignore_ascii_case(&mv.mtype));
    let stab = if has_stab {
        if ability_is(atk, "adaptability") { 2.0 } else { 1.5 }
    } else {
        1.0
    };
    let crit_m = if crit { 1.5 } else { 1.0 };
    let base = ((22.0 * pow * (off_val / def_val)) / 50.0) + 2.0;
    let dmg = (base * stab * eff * crit_m * roll).round().max(1.0) as i32;
    (dmg, eff, None)
}

enum OppDecision {
    Move(usize),
    HealPotion,
    Switch(usize),
}

fn choose_grandmaster_action(
    opp_team: &[Fighter],
    e_idx: usize,
    player: &Fighter,
    opp_potions: u8,
) -> OppDecision {
    let opp = &opp_team[e_idx];
    if let Some(ch_idx) = opp.charging_move {
        return OppDecision::Move(ch_idx);
    }

    // Estimate worst threat player can deal right now
    let player_max_threat = player
        .moves
        .iter()
        .filter(|m| !m.is_status && m.pp > 0)
        .map(|m| calc_move_damage(m, player, opp, false, 0.95).0)
        .max()
        .unwrap_or(50);

    // Check if Opponent has a guaranteed KO first!
    let mut best_ko: Option<(usize, i32)> = None;
    for (i, mv) in opp.moves.iter().enumerate() {
        if mv.pp <= 0 || mv.is_status || mv.charge_turn {
            continue;
        }
        let (dmg, eff, ab) = calc_move_damage(mv, opp, player, false, 0.88);
        if ab.is_none() && eff > 0.0 && dmg >= player.hp {
            let ko_score = mv.priority * 1000 + mv.accuracy * 5 + dmg;
            if best_ko.map(|(_, s)| ko_score > s).unwrap_or(true) {
                best_ko = Some((i, ko_score));
            }
        }
    }
    if let Some((idx, _)) = best_ko {
        return OppDecision::Move(idx);
    }

    // Tactical switch in team battles if severely threatened and a better teammate exists
    if opp_team.iter().filter(|f| f.hp > 0).count() > 1 && player_max_threat >= opp.hp && effective_speed(player) > effective_speed(opp) {
        let mut best_sw: Option<(usize, i32)> = None;
        for (i, cand) in opp_team.iter().enumerate() {
            if i == e_idx || cand.hp <= cand.max_hp / 3 {
                continue;
            }
            let threat = player
                .moves
                .iter()
                .filter(|m| !m.is_status)
                .map(|m| calc_move_damage(m, player, cand, false, 0.95).0)
                .max()
                .unwrap_or(50);
            if threat < cand.hp / 2 {
                let sc = cand.hp - threat;
                if best_sw.map(|(_, s)| sc > s).unwrap_or(true) {
                    best_sw = Some((i, sc));
                }
            }
        }
        if let Some((sw_i, _)) = best_sw {
            return OppDecision::Switch(sw_i);
        }
    }

    // Tactical Hyper Potion when critically low and healing lets Opponent survive
    if opp_potions > 0 && opp.hp * 100 <= opp.max_hp * 34 && (opp.hp + 120).min(opp.max_hp) > player_max_threat {
        return OppDecision::HealPotion;
    }

    // Score every legal move with Grandmaster heuristics
    let opp_faster = effective_speed(opp) >= effective_speed(player);
    let mut best_idx = 0usize;
    let mut best_score = -10_000i32;

    for (i, mv) in opp.moves.iter().enumerate() {
        if mv.pp <= 0 {
            continue;
        }
        let mut score: i32;
        if mv.is_status {
            score = -50;
            if mv.healing > 0 {
                if opp.hp * 100 <= opp.max_hp * 48 && (opp.hp + opp.max_hp / 2).min(opp.max_hp) > player_max_threat {
                    score = 185;
                }
            } else if mv.target_self && !mv.stat_changes.is_empty() {
                let raises_offense = mv.stat_changes.iter().any(|(s, d)| {
                    *d > 0 && matches!(s.as_str(), "attack" | "special-attack" | "speed")
                });
                let total_boost: i32 = opp.stages.iter().sum();
                if raises_offense && total_boost < 3 && opp.hp > player_max_threat * 2 {
                    score = 135;
                }
            } else if !mv.target_self && player.status == Status::None && player.hp * 100 > player.max_hp * 45 {
                match mv.ailment.as_str() {
                    "sleep" => score = 155,
                    "burn" if player.atk >= player.spa => score = 145,
                    "paralysis" if !opp_faster => score = 145,
                    "poison" if player.max_hp >= 220 => score = 125,
                    _ => {}
                }
            }
        } else {
            let (dmg, eff, ab) = calc_move_damage(mv, opp, player, false, 0.93);
            if ab.is_some() || eff == 0.0 {
                score = -5000;
            } else {
                let expected = (dmg as f32 * (mv.accuracy as f32 / 100.0)) as i32;
                score = expected;
                if mv.drain > 0 && opp.hp < opp.max_hp {
                    score += (dmg * mv.drain / 160).min(45);
                }
                if mv.drain < 0 && (dmg * (-mv.drain) / 100) >= opp.hp {
                    score -= 80;
                }
                if mv.priority > 0 && !opp_faster && opp.hp <= player_max_threat {
                    score += 45;
                }
                if mv.charge_turn || mv.recharge_turn {
                    score -= 55;
                }
            }
        }

        if score > best_score {
            best_score = score;
            best_idx = i;
        }
    }

    OppDecision::Move(best_idx)
}

// =========================================================================
// STAGE RENDERING (CRISP, ZERO OVERFLOW, LIVE IDLE ANIMATIONS)
// =========================================================================

fn format_stages(stages: &[i32; 5]) -> String {
    let labels = ["ATK", "DEF", "SPA", "SPD", "SPE"];
    let mut parts = Vec::new();
    for (i, &s) in stages.iter().enumerate() {
        if s > 0 { parts.push(format!("{}+{}", labels[i], s)); }
        else if s < 0 { parts.push(format!("{}{}", labels[i], s)); }
    }
    parts.join(" ")
}

fn draw_stage(
    player: &mut Fighter,
    enemy: &mut Fighter,
    counts: (usize, usize, usize, usize),
    pose: &StagePose,
) {
    let (tw, th) = term_dims();
    let box_w = tw.saturating_sub(3).clamp(32, 92);
    let inner = box_w - 2;
    let cw = inner - 2;
    let gold = (255, 215, 70);
    let pc = type_rgb(player.types.first().map(|s| s.as_str()).unwrap_or("cyan"));
    let ec = type_rgb(enemy.types.first().map(|s| s.as_str()).unwrap_or("magenta"));
    let bv = "│".truecolor(gold.0, gold.1, gold.2).to_string();

    let mut buf = String::with_capacity(12288);
    buf.push_str("\x1b[H");
    buf.push_str(&format!(
        " {}\x1b[K\r\n",
        format!("╭{}╮", "─".repeat(inner)).truecolor(gold.0, gold.1, gold.2)
    ));

    // Top status strip (Mode + Streak + BGM indicator)
    let mode_lbl = if pose.ranked {
        format!("RANKED RANDOM  |  STREAK: {}", pose.streak)
    } else {
        "CUSTOM BATTLE".to_string()
    };
    let bgm_lbl = if pose.muted { "[M] BGM:OFF" } else { "[M] BGM:ON" };
    let top_strip = if mode_lbl.len() + bgm_lbl.len() + 2 <= cw {
        format!(
            "{}{}{}",
            mode_lbl,
            " ".repeat(cw - mode_lbl.len() - bgm_lbl.len()),
            bgm_lbl
        )
    } else {
        pad_right(&mode_lbl, cw)
    };
    buf.push_str(&format!(
        " {} {} {}\x1b[K\r\n",
        bv,
        top_strip.truecolor(130, 225, 255).bold(),
        bv
    ));
    buf.push_str(&format!(
        " {}\x1b[K\r\n",
        format!("├{}┤", "─".repeat(inner)).truecolor(gold.0, gold.1, gold.2)
    ));

    // Opponent HUD
    let hp_w = if cw >= 48 { 14 } else { 8 };
    let berry_tag = if enemy.sitrus_berry { " [Berry]" } else { "" };
    let e_head = format!(
        "OPPONENT [{}/{}] {} Lv.50{} ({}){}",
        counts.2, counts.3, enemy.name, enemy.status.tag(), enemy.ability, berry_tag
    );
    let e_hp_plain = format!("HP [{}] {}/{}", " ".repeat(hp_w), enemy.hp, enemy.max_hp);
    let e_st_raw = format_stages(&enemy.stages);
    let e_st_space = cw.saturating_sub(e_hp_plain.len() + 2);
    let e_st: String = e_st_raw.chars().take(e_st_space).collect();
    let e_hp_colored = format!("HP {} {}/{}", hp_bar(enemy.hp, enemy.max_hp, hp_w), enemy.hp, enemy.max_hp);
    let (e_sub, e_vis_len) = if e_st.is_empty() {
        (e_hp_colored, e_hp_plain.len())
    } else {
        (format!("{}  {}", e_hp_colored, e_st.yellow()), e_hp_plain.len() + 2 + e_st.len())
    };
    buf.push_str(&format!(
        " {} {} {}\x1b[K\r\n",
        bv,
        pad_right(&e_head, cw).truecolor(ec.0, ec.1, ec.2).bold(),
        bv
    ));
    buf.push_str(&format!(
        " {} {}{} {}\x1b[K\r\n",
        bv,
        e_sub,
        " ".repeat(cw.saturating_sub(e_vis_len)),
        bv
    ));

    // Animated Dual-Sprite Arena
    let msg_rows = wrap_text(pose.msg, cw).len().min(2)
        + if pose.sub_msg.is_empty() { 0 } else { wrap_text(pose.sub_msg, cw).len().min(2) };
    let log_rows: usize = pose.log_lines.iter().map(|l| wrap_text(l, cw).len().min(2)).sum();
    let fixed_overhead = 10 + msg_rows + pose.fx_lines.len().max(1)
        + if log_rows > 0 { log_rows + 1 } else { 0 }
        + if !pose.menu_lines.is_empty() { pose.menu_lines.len() + 1 } else { 0 };
    let avail_arena = th.saturating_sub(fixed_overhead);

    if avail_arena >= 4 && cw >= 28 {
        let sp_w = ((cw.saturating_sub(6)) / 2).clamp(10, 26) as u32;
        let max_sp_rows = avail_arena.clamp(4, 13);
        player.ensure_frames(sp_w, max_sp_rows);
        enemy.ensure_frames(sp_w, max_sp_rows);

        let p_frame_idx = pose.anim_tick % player.cached_frames.len().max(1);
        let e_frame_idx = (pose.anim_tick + 2) % enemy.cached_frames.len().max(1);

        // Gentle breathing bob for 1-frame fallback sprites
        let p_idle_bob = if player.cached_frames.len() == 1 && (pose.anim_tick / 4) % 2 == 1 { 1 } else { 0 };
        let e_idle_bob = if enemy.cached_frames.len() == 1 && ((pose.anim_tick + 2) / 4) % 2 == 1 { 1 } else { 0 };

        let pw = player.cached_w.min(cw / 2);
        let ew = enemy.cached_w.min(cw / 2);
        let p_lines: &[String] = if pose.hide_p { &[] } else { &player.cached_frames[p_frame_idx] };
        let e_lines: &[String] = if pose.hide_e { &[] } else { &enemy.cached_frames[e_frame_idx] };

        let p_show_len = p_lines.len().min(max_sp_rows);
        let e_show_len = e_lines.len().min(max_sp_rows);
        let arena_h = p_show_len.max(e_show_len).max(4).min(max_sp_rows);

        let avail = cw.saturating_sub(pw + ew);
        let base_pad = (avail / 6).min(3);
        let p_lp = ((base_pad as isize + pose.p_shift).clamp(0, avail as isize)) as usize;
        let rem_after_p = avail.saturating_sub(p_lp);
        let e_rp = ((base_pad as isize - pose.e_shift).clamp(0, rem_after_p as isize)) as usize;
        let mid_gap = avail.saturating_sub(p_lp + e_rp);

        for r in 0..arena_h {
            let p_top = arena_h
                .saturating_sub(p_show_len)
                .saturating_sub(pose.p_hop + p_idle_bob)
                + pose.p_drop;
            let e_top = (pose.e_drop).saturating_sub(pose.e_hop + e_idle_bob);

            let left_draw = r
                .checked_sub(p_top)
                .filter(|&i| i < p_show_len)
                .map(|i| p_lines[i].as_str())
                .unwrap_or("");
            let right_draw = r
                .checked_sub(e_top)
                .filter(|&i| i < e_show_len)
                .map(|i| e_lines[i].as_str())
                .unwrap_or("");

            let l_out = if left_draw.is_empty() { " ".repeat(pw) } else { left_draw.to_string() };
            let r_out = if right_draw.is_empty() { " ".repeat(ew) } else { right_draw.to_string() };

            buf.push_str(&format!(
                " {} {}{}{}{}{} {}\x1b[K\r\n",
                bv,
                " ".repeat(p_lp),
                l_out,
                " ".repeat(mid_gap),
                r_out,
                " ".repeat(e_rp),
                bv
            ));
        }

        // Ground shadow platforms under both Pokemon
        let p_plat = if pose.hide_p { " ".repeat(pw) } else { "═".repeat(pw).truecolor(pc.0 / 2, pc.1 / 2, pc.2 / 2).to_string() };
        let e_plat = if pose.hide_e { " ".repeat(ew) } else { "═".repeat(ew).truecolor(ec.0 / 2, ec.1 / 2, ec.2 / 2).to_string() };
        buf.push_str(&format!(
            " {} {}{}{}{}{} {}\x1b[K\r\n",
            bv,
            " ".repeat(p_lp),
            p_plat,
            " ".repeat(mid_gap),
            e_plat,
            " ".repeat(e_rp),
            bv
        ));
    }

    // Attack FX strip
    if !pose.fx_lines.is_empty() {
        for fx in pose.fx_lines {
            buf.push_str(&format!(
                " {} {} {}\x1b[K\r\n",
                bv,
                pad_right(fx, cw).truecolor(pose.fx_col.0, pose.fx_col.1, pose.fx_col.2).bold(),
                bv
            ));
        }
    } else {
        buf.push_str(&format!(" {} {} {}\x1b[K\r\n", bv, " ".repeat(cw), bv));
    }

    // Player HUD
    let p_head = format!(
        "YOU [{}/{}] {} Lv.50{} ({})",
        counts.0, counts.1, player.name, player.status.tag(), player.ability
    );
    let p_hp_plain = format!("HP [{}] {}/{}", " ".repeat(hp_w), player.hp, player.max_hp);
    let p_st_raw = format_stages(&player.stages);
    let p_st_space = cw.saturating_sub(p_hp_plain.len() + 2);
    let p_st: String = p_st_raw.chars().take(p_st_space).collect();
    let p_hp_colored = format!("HP {} {}/{}", hp_bar(player.hp, player.max_hp, hp_w), player.hp, player.max_hp);
    let (p_sub, p_vis_len) = if p_st.is_empty() {
        (p_hp_colored, p_hp_plain.len())
    } else {
        (format!("{}  {}", p_hp_colored, p_st.yellow()), p_hp_plain.len() + 2 + p_st.len())
    };
    buf.push_str(&format!(
        " {} {} {}\x1b[K\r\n",
        bv,
        pad_right(&p_head, cw).truecolor(pc.0, pc.1, pc.2).bold(),
        bv
    ));
    buf.push_str(&format!(
        " {} {}{} {}\x1b[K\r\n",
        bv,
        p_sub,
        " ".repeat(cw.saturating_sub(p_vis_len)),
        bv
    ));

    // Message box
    buf.push_str(&format!(
        " {}\x1b[K\r\n",
        format!("├{}┤", "─".repeat(inner)).truecolor(gold.0, gold.1, gold.2)
    ));
    for line in wrap_text(pose.msg, cw).into_iter().take(2) {
        buf.push_str(&format!(
            " {} {} {}\x1b[K\r\n",
            bv,
            pad_right(&line, cw).truecolor(242, 246, 255).bold(),
            bv
        ));
    }
    if !pose.sub_msg.is_empty() {
        for line in wrap_text(pose.sub_msg, cw).into_iter().take(2) {
            buf.push_str(&format!(
                " {} {} {}\x1b[K\r\n",
                bv,
                pad_right(&line, cw).truecolor(170, 195, 225),
                bv
            ));
        }
    }

    // Live Battle Log panel
    if !pose.log_lines.is_empty() {
        buf.push_str(&format!(
            " {}\x1b[K\r\n",
            format!("├{}┤", "─".repeat(inner)).truecolor(gold.0, gold.1, gold.2)
        ));
        for lg in pose.log_lines {
            for line in wrap_text(lg, cw).into_iter().take(2) {
                buf.push_str(&format!(
                    " {} {} {}\x1b[K\r\n",
                    bv,
                    pad_right(&line, cw).truecolor(185, 202, 220),
                    bv
                ));
            }
        }
    }

    // Interactive Menu panel (ALWAYS includes closing bv)
    if !pose.menu_lines.is_empty() {
        buf.push_str(&format!(
            " {}\x1b[K\r\n",
            format!("├{}┤", "─".repeat(inner)).truecolor(gold.0, gold.1, gold.2)
        ));
        for ml in pose.menu_lines {
            buf.push_str(&format!(" {} {} {}\x1b[K\r\n", bv, ml, bv));
        }
    }

    buf.push_str(&format!(
        " {}\x1b[K\r\n\x1b[J",
        format!("╰{}╯", "─".repeat(inner)).truecolor(gold.0, gold.1, gold.2)
    ));

    let mut out = io::stdout();
    let _ = out.write_all(buf.as_bytes());
    let _ = out.flush();
}

// Non-blocking key wait that continuously runs idle animations!
fn wait_key_animated(
    player: &mut Fighter,
    enemy: &mut Fighter,
    counts: (usize, usize, usize, usize),
    msg: &str,
    sub_msg: &str,
    log_lines: &[String],
    menu_lines: &[String],
    bgm: &BgmPlayer,
    ranked: bool,
    streak: u32,
    tick: &mut usize,
) -> Option<KeyCode> {
    let mut last_draw = Instant::now() - Duration::from_secs(1);
    loop {
        if last_draw.elapsed() >= Duration::from_millis(95) {
            let pose = StagePose {
                msg,
                sub_msg,
                log_lines,
                menu_lines,
                anim_tick: *tick,
                muted: bgm.is_muted(),
                ranked,
                streak,
                ..Default::default()
            };
            draw_stage(player, enemy, counts, &pose);
            *tick = tick.wrapping_add(1);
            last_draw = Instant::now();
        }

        if event::poll(Duration::from_millis(35)).unwrap_or(false) {
            if let Ok(ev) = event::read() {
                match ev {
                    Event::Key(k) if k.kind == KeyEventKind::Press => {
                        if k.modifiers.contains(KeyModifiers::CONTROL)
                            && matches!(k.code, KeyCode::Char('c') | KeyCode::Char('C'))
                        {
                            return None;
                        }
                        if matches!(k.code, KeyCode::Char('m') | KeyCode::Char('M')) {
                            bgm.toggle_mute();
                            last_draw = Instant::now() - Duration::from_secs(1);
                            continue;
                        }
                        return Some(k.code);
                    }
                    Event::Resize(_, _) => {
                        last_draw = Instant::now() - Duration::from_secs(1);
                    }
                    _ => {}
                }
            }
        }
    }
}

// =========================================================================
// CRISP TYPE-SPECIFIC ATTACK, DAMAGE, KO, CAPTURE & VICTORY ANIMATIONS
// =========================================================================

fn build_attack_fx(mtype: &str, is_status: bool, p_attacking: bool, step: usize, width: usize) -> Vec<String> {
    let w = width.clamp(24, 86);
    if is_status {
        let frames = [
            ("  *     +     *  ", "    +   @   +    "),
            ("    +   @   +    ", "  *  ◈  ★  ◈  *  "),
            ("*  ◈  ★  ◈  ★  *", "  +   *   @   +  "),
            ("    ◈   ★   ◈    ", "      *   *      "),
            ("      +   +      ", "        ★        "),
        ];
        let (a, b) = frames[step % frames.len()];
        return vec![center_str(a, w), center_str(b, w)];
    }

    let (top, mid, bot, impact) = match mtype.to_lowercase().as_str() {
        "fire" => ("*▲*▲*", "▲▲***▲▲▲***", "*▲*▲*", "✦ ══ BLAZE IMPACT! ══ ✦"),
        "water" => ("~≈~≈~", "≈≈~~~≈≈≈~~~", "~≈~≈~", "✦ ══ HYDRO SURGE! ══ ✦"),
        "electric" => ("/\\/\\*", "»»/\\/\\/\\««", "*\\/\\/", "✦ ══ THUNDER STRIKE! ══ ✦"),
        "grass" => ("*///*", "///****///", "*///*", "✦ ══ RAZOR SLASH! ══ ✦"),
        "ice" => ("*♦*♦*", "♦♦**♦♦♦**♦♦", "*♦*♦*", "✦ ══ FROST SHATTER! ══ ✦"),
        "poison" => ("°o°o°", "●○●○○●○●", "°o°o°", "✦ ══ TOXIC BURST! ══ ✦"),
        "ground" | "rock" => ("▲▓▲▓▲", "▓▓▓▲▲▲▓▓▓", "▲▓▲▓▲", "✦ ══ QUAKE CRUSH! ══ ✦"),
        "ghost" | "dark" | "psychic" => ("◎░◎░◎", "░◎◉◎◉◎◉◎░", "◎░◎░◎", "✦ ══ PSYCHIC WAVE! ══ ✦"),
        "dragon" | "fairy" => ("★✦★✦★", "══★✦★✦★══", "★✦★✦★", "✦ ══ MYTHIC BURST! ══ ✦"),
        "fighting" | "steel" => ("»✖»✖»", "»»»✖✖✖»»»", "»✖»✖»", "✦ ══ HEAVY IMPACT! ══ ✦"),
        _ => ("»*»*»", "»»»✦✦✦»»»", "»*»*»", "✦ ══ DIRECT HIT! ══ ✦"),
    };

    if step >= 5 {
        return vec![
            center_str("✦   *   ★   *   ✦", w),
            center_str(impact, w),
        ];
    }

    let travel = w.saturating_sub(14);
    let frac = (step as f32) / 4.0;
    let pos = if p_attacking {
        (frac * travel as f32).round() as usize
    } else {
        ((1.0 - frac) * travel as f32).round() as usize
    };
    let pad = " ".repeat(pos);
    vec![
        format!("{} {}", pad, top),
        format!("{}{}", pad, mid),
        format!("{} {}", pad, bot),
    ]
}

fn animate_attack(
    player: &mut Fighter,
    enemy: &mut Fighter,
    counts: (usize, usize, usize, usize),
    mv: &BattleMove,
    attacker_name: &str,
    p_attacking: bool,
    bgm: &BgmPlayer,
    ranked: bool,
    streak: u32,
    tick: &mut usize,
) {
    let (tw, _) = term_dims();
    let cw = tw.saturating_sub(7).clamp(28, 88);
    let col = type_rgb(&mv.mtype);
    let cat = if mv.is_status { "Status" } else if mv.is_physical { "Phys" } else { "Spec" };
    let side = if p_attacking { "YOU" } else { "OPPONENT" };
    let msg = format!("[{}] {} used {}! ({}/{} {}BP)", side, attacker_name, mv.name, mv.mtype, cat, mv.power);
    let sub = format!("Effect: {}", mv.desc);

    for step in 0..6 {
        let fx = build_attack_fx(&mv.mtype, mv.is_status, p_attacking, step, cw);
        let p_shift = if p_attacking && !mv.is_status { if step == 1 || step == 2 { 4 } else { 1 } } else { 0 };
        let e_shift = if !p_attacking && !mv.is_status { if step == 1 || step == 2 { -4 } else { -1 } } else { 0 };
        let p_hop = if p_attacking && mv.is_status && step % 2 == 1 { 1 } else { 0 };
        let e_hop = if !p_attacking && mv.is_status && step % 2 == 1 { 1 } else { 0 };
        *tick = tick.wrapping_add(1);
        let pose = StagePose {
            fx_lines: &fx,
            fx_col: col,
            msg: &msg,
            sub_msg: &sub,
            p_shift,
            e_shift,
            p_hop,
            e_hop,
            anim_tick: *tick,
            muted: bgm.is_muted(),
            ranked,
            streak,
            ..Default::default()
        };
        draw_stage(player, enemy, counts, &pose);
        thread::sleep(Duration::from_millis(110));
    }
}

fn animate_damage(
    player: &mut Fighter,
    enemy: &mut Fighter,
    counts: (usize, usize, usize, usize),
    target_is_enemy: bool,
    dmg: i32,
    msg: &str,
    sub: &str,
    bgm: &BgmPlayer,
    ranked: bool,
    streak: u32,
    tick: &mut usize,
) {
    let (tw, _) = term_dims();
    let cw = tw.saturating_sub(7).clamp(28, 88);
    let col = (255, 95, 95);
    let fx = vec![center_str(&format!("✖  -{} HP!  ✖", dmg), cw)];

    for blink in 0..4 {
        let hide = blink % 2 == 0;
        let shake: isize = if blink % 2 == 0 { 2 } else { -2 };
        *tick = tick.wrapping_add(1);
        let pose = StagePose {
            fx_lines: &fx,
            fx_col: col,
            msg,
            sub_msg: sub,
            hide_p: !target_is_enemy && hide,
            hide_e: target_is_enemy && hide,
            p_shift: if !target_is_enemy { shake } else { 0 },
            e_shift: if target_is_enemy { shake } else { 0 },
            anim_tick: *tick,
            muted: bgm.is_muted(),
            ranked,
            streak,
            ..Default::default()
        };
        draw_stage(player, enemy, counts, &pose);
        thread::sleep(Duration::from_millis(85));
    }

    let start_hp = if target_is_enemy { enemy.hp } else { player.hp };
    let end_hp = (start_hp - dmg).max(0);
    let steps = 8;
    for s in 1..=steps {
        let cur = start_hp - ((start_hp - end_hp) * s / steps);
        if target_is_enemy { enemy.hp = cur; } else { player.hp = cur; }
        *tick = tick.wrapping_add(1);
        let pose = StagePose {
            fx_lines: &fx,
            fx_col: col,
            msg,
            sub_msg: sub,
            anim_tick: *tick,
            muted: bgm.is_muted(),
            ranked,
            streak,
            ..Default::default()
        };
        draw_stage(player, enemy, counts, &pose);
        thread::sleep(Duration::from_millis(50));
    }
    if target_is_enemy { enemy.hp = end_hp; } else { player.hp = end_hp; }
    thread::sleep(Duration::from_millis(550));
}

fn animate_ko(
    player: &mut Fighter,
    enemy: &mut Fighter,
    counts: (usize, usize, usize, usize),
    enemy_ko: bool,
    bgm: &BgmPlayer,
    ranked: bool,
    streak: u32,
    tick: &mut usize,
) {
    let (tw, _) = term_dims();
    let cw = tw.saturating_sub(7).clamp(28, 88);
    let (fid, fname, fcry) = if enemy_ko {
        (enemy.id, enemy.name.clone(), enemy.data["cry_url"].as_str().unwrap_or("").to_string())
    } else {
        (player.id, player.name.clone(), player.data["cry_url"].as_str().unwrap_or("").to_string())
    };
    let cry = if !bgm.is_muted() { spawn_play_cry(fid, fname.clone(), fcry) } else { None };
    let msg = format!("{} fainted!", fname);
    let fx = vec![center_str("✖  KNOCKOUT!  ✖", cw)];

    for step in 1..=5 {
        *tick = tick.wrapping_add(1);
        let pose = StagePose {
            fx_lines: &fx,
            fx_col: (255, 80, 80),
            msg: &msg,
            p_drop: if !enemy_ko { step } else { 0 },
            e_drop: if enemy_ko { step } else { 0 },
            anim_tick: *tick,
            muted: bgm.is_muted(),
            ranked,
            streak,
            ..Default::default()
        };
        draw_stage(player, enemy, counts, &pose);
        thread::sleep(Duration::from_millis(100));
    }

    let final_pose = StagePose {
        fx_lines: &fx,
        fx_col: (255, 80, 80),
        msg: &msg,
        hide_p: !enemy_ko,
        hide_e: enemy_ko,
        anim_tick: *tick,
        muted: bgm.is_muted(),
        ranked,
        streak,
        ..Default::default()
    };
    draw_stage(player, enemy, counts, &final_pose);
    if let Some(h) = cry { let _ = h.join(); }
    thread::sleep(Duration::from_millis(500));
}

fn animate_pokeball(
    player: &mut Fighter,
    enemy: &mut Fighter,
    counts: (usize, usize, usize, usize),
    caught: bool,
    bgm: &BgmPlayer,
    ranked: bool,
    streak: u32,
    tick: &mut usize,
) {
    let (tw, _) = term_dims();
    let cw = tw.saturating_sub(7).clamp(28, 88);
    let frames = [
        ("●   (throwing Ultra Ball...)", (255, 220, 70)),
        ("        ●", (255, 220, 70)),
        ("                ●", (255, 220, 70)),
        ("        (o)  *wiggle left*", (255, 215, 70)),
        ("        (o)  *wiggle right*", (255, 215, 70)),
        ("        (o)  *wiggle left*", (255, 215, 70)),
    ];
    for (i, (fx, col)) in frames.iter().enumerate() {
        *tick = tick.wrapping_add(1);
        let lines = vec![center_str(fx, cw)];
        let pose = StagePose {
            fx_lines: &lines,
            fx_col: *col,
            msg: "You threw an Ultra Ball!",
            hide_e: i >= 3,
            anim_tick: *tick,
            muted: bgm.is_muted(),
            ranked,
            streak,
            ..Default::default()
        };
        draw_stage(player, enemy, counts, &pose);
        thread::sleep(Duration::from_millis(if i < 3 { 140 } else { 320 }));
    }

    let (fx, col, msg) = if caught {
        (
            vec![center_str("✦ ★ GOTCHA! CAPTURED! ★ ✦", cw)],
            (90, 235, 115),
            format!("{} was caught and added to your PC Box!", enemy.name),
        )
    } else {
        (
            vec![center_str("! IT BROKE FREE !", cw)],
            (255, 110, 90),
            format!("Oh no! {} broke free from the Ultra Ball!", enemy.name),
        )
    };
    let pose = StagePose {
        fx_lines: &fx,
        fx_col: col,
        msg: &msg,
        hide_e: caught,
        anim_tick: *tick,
        muted: bgm.is_muted(),
        ranked,
        streak,
        ..Default::default()
    };
    draw_stage(player, enemy, counts, &pose);
    thread::sleep(Duration::from_millis(900));
}

fn animate_victory(
    player: &mut Fighter,
    enemy: &mut Fighter,
    counts: (usize, usize, usize, usize),
    player_won: bool,
    stats: &MatchStats,
    bgm: &BgmPlayer,
    ranked: bool,
    new_streak: u32,
    best_streak: u32,
    tick: &mut usize,
) {
    let was_muted = bgm.is_muted();
    bgm.stop();
    if player_won {
        play_victory_fanfare(was_muted);
    }

    let (wid, wname, wcry, wtype) = if player_won {
        (
            player.id,
            player.name.clone(),
            player.data["cry_url"].as_str().unwrap_or("").to_string(),
            player.types.first().cloned().unwrap_or_else(|| "gold".to_string()),
        )
    } else {
        (
            enemy.id,
            enemy.name.clone(),
            enemy.data["cry_url"].as_str().unwrap_or("").to_string(),
            enemy.types.first().cloned().unwrap_or_else(|| "gold".to_string()),
        )
    };
    let cry = if !was_muted { spawn_play_cry(wid, wname.clone(), wcry) } else { None };
    let wc = type_rgb(&wtype);

    let title = if player_won {
        format!("CHAMPION VICTORY! {} defeated {}!", player.name, enemy.name)
    } else {
        format!("DEFEAT! OPPONENT's {} knocked out your team.", enemy.name)
    };

    let fireworks = [
        ("  ★   ✦   *   ★   ✦   *   ★  ", "    *  ★  CHAMPION!  ★  *    "),
        ("   ✦   *   ★   ✦   *   ★   ✦ ", "  ✦  *  ★  VICTORY!  ★  *  ✦ "),
        ("  *   ★   ✦   *   ★   ✦   *  ", "   ★  ✦  *  WINNER!  *  ✦  ★ "),
        ("   ★   ✦   *   ★   ✦   *   ★ ", "  *  ★  ✦  CHAMPION!  ✦  ★  *"),
    ];

    let mut frame_i = 0usize;
    let mut last_draw = Instant::now() - Duration::from_secs(1);
    loop {
        if last_draw.elapsed() >= Duration::from_millis(220) {
            let (tw, _) = term_dims();
            let cw = tw.saturating_sub(7).clamp(28, 88);
            let streak_line = if ranked {
                format!("RANKED STREAK: {} (BEST: {})  |  Run: pokefact --dispstreak", new_streak, best_streak)
            } else {
                "Custom Battle Complete  |  Try Ranked: pokefact --battle <pokemon>".to_string()
            };
            let summary = vec![
                pad_right(&format!("Turns: {} | Damage: {} HP | Super: {} | Crits: {}", stats.turns, stats.total_dmg, stats.super_hits, stats.crits), cw)
                    .truecolor(255, 230, 120)
                    .bold()
                    .to_string(),
                pad_right(&streak_line, cw)
                    .truecolor(90, 235, 115)
                    .bold()
                    .to_string(),
                pad_right("Press [Enter] or [Q] to exit arena...", cw).dimmed().to_string(),
            ];
            let (f1, f2) = fireworks[frame_i % fireworks.len()];
            let fx = vec![center_str(f1, cw), center_str(f2, cw)];
            let hop = if frame_i % 2 == 0 { 1 } else { 0 };
            *tick = tick.wrapping_add(1);
            let pose = StagePose {
                fx_lines: &fx,
                fx_col: if player_won { wc } else { (255, 95, 95) },
                msg: &title,
                menu_lines: &summary,
                hide_p: !player_won,
                hide_e: player_won,
                p_hop: if player_won { hop } else { 0 },
                e_hop: if !player_won { hop } else { 0 },
                anim_tick: *tick,
                muted: was_muted,
                ranked,
                streak: new_streak,
                ..Default::default()
            };
            draw_stage(player, enemy, counts, &pose);
            frame_i += 1;
            last_draw = Instant::now();
        }
        if event::poll(Duration::from_millis(40)).unwrap_or(false) {
            if let Ok(Event::Key(k)) = event::read() {
                if k.kind == KeyEventKind::Press
                    && matches!(k.code, KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Char(' '))
                {
                    break;
                }
            }
        }
    }
    if let Some(h) = cry { let _ = h.join(); }
}

// =========================================================================
// TURN RESOLUTION
// =========================================================================

fn apply_stat_changes(target: &mut Fighter, changes: &[(String, i32)]) -> String {
    let mut notes = Vec::new();
    for (sn, delta) in changes {
        let idx = match sn.as_str() {
            "attack" => Some(0),
            "defense" => Some(1),
            "special-attack" => Some(2),
            "special-defense" => Some(3),
            "speed" => Some(4),
            _ => None,
        };
        if let Some(i) = idx {
            let before = target.stages[i];
            target.stages[i] = (target.stages[i] + delta).clamp(-6, 6);
            if target.stages[i] != before {
                notes.push(format!("{} {}{}", sn.replace('-', " ").capitalize(), if *delta > 0 { "+" } else { "" }, delta));
            }
        }
    }
    notes.join(", ")
}

fn push_logs(recent: &mut Vec<String>, full: &mut Vec<String>, entry: String) {
    full.push(entry.clone());
    recent.push(entry);
    if recent.len() > 2 {
        recent.remove(0);
    }
}

fn resolve_turn_action(
    player: &mut Fighter,
    enemy: &mut Fighter,
    p_attacking: bool,
    move_idx: usize,
    counts: (usize, usize, usize, usize),
    stats: &mut MatchStats,
    recent_log: &mut Vec<String>,
    full_log: &mut Vec<String>,
    bgm: &BgmPlayer,
    ranked: bool,
    streak: u32,
    tick: &mut usize,
) {
    let side = if p_attacking { "YOU" } else { "OPPONENT" };
    let (tw, _) = term_dims();
    let cw = tw.saturating_sub(7).clamp(28, 88);

    // Handle Recharge turn (Hyper Beam / Giga Impact)
    {
        let atk = if p_attacking { &mut *player } else { &mut *enemy };
        if atk.must_recharge {
            atk.must_recharge = false;
            let msg = format!("[{}] {} must recharge this turn!", side, atk.name);
            let fx = vec![center_str("... RECHARGING ...", cw)];
            let pose = StagePose { fx_lines: &fx, fx_col: (190, 195, 210), msg: &msg, anim_tick: *tick, muted: bgm.is_muted(), ranked, streak, ..Default::default() };
            draw_stage(player, enemy, counts, &pose);
            push_logs(recent_log, full_log, msg);
            thread::sleep(Duration::from_millis(750));
            return;
        }
    }

    // Handle Status blockers (Sleep / Freeze / Paralyze)
    let r = rand_seed();
    {
        let atk = if p_attacking { &mut *player } else { &mut *enemy };
        match atk.status {
            Status::Sleep(rem) => {
                if rem <= 1 {
                    atk.status = Status::None;
                } else {
                    atk.status = Status::Sleep(rem - 1);
                    let msg = format!("[{}] {} is fast asleep!", side, atk.name);
                    let fx = vec![center_str("zZz... ASLEEP ...zZz", cw)];
                    let pose = StagePose { fx_lines: &fx, fx_col: (140, 180, 255), msg: &msg, anim_tick: *tick, muted: bgm.is_muted(), ranked, streak, ..Default::default() };
                    draw_stage(player, enemy, counts, &pose);
                    push_logs(recent_log, full_log, msg);
                    thread::sleep(Duration::from_millis(750));
                    return;
                }
            }
            Status::Freeze => {
                if (r % 5) == 0 {
                    atk.status = Status::None;
                } else {
                    let msg = format!("[{}] {} is frozen solid!", side, atk.name);
                    let fx = vec![center_str("* FROZEN SOLID *", cw)];
                    let pose = StagePose { fx_lines: &fx, fx_col: (120, 230, 245), msg: &msg, anim_tick: *tick, muted: bgm.is_muted(), ranked, streak, ..Default::default() };
                    draw_stage(player, enemy, counts, &pose);
                    push_logs(recent_log, full_log, msg);
                    thread::sleep(Duration::from_millis(750));
                    return;
                }
            }
            Status::Paralyze if (r % 4) == 0 => {
                let msg = format!("[{}] {} is paralyzed and can't move!", side, atk.name);
                let fx = vec![center_str("✦ FULLY PARALYZED ✦", cw)];
                let pose = StagePose { fx_lines: &fx, fx_col: (255, 215, 45), msg: &msg, anim_tick: *tick, muted: bgm.is_muted(), ranked, streak, ..Default::default() };
                draw_stage(player, enemy, counts, &pose);
                push_logs(recent_log, full_log, msg);
                thread::sleep(Duration::from_millis(750));
                return;
            }
            _ => {}
        }
    }

    // Handle 2-turn Charge moves (e.g. Solar Beam)
    let mv = {
        let atk = if p_attacking { &mut *player } else { &mut *enemy };
        let Some(m) = atk.moves.get_mut(move_idx) else { return; };
        if m.charge_turn && atk.charging_move != Some(move_idx) {
            atk.charging_move = Some(move_idx);
            let cname = m.name.clone();
            let ctype = m.mtype.clone();
            let msg = format!("[{}] {} began charging {}!", side, atk.name, cname);
            let fx = vec![center_str("✦ CHARGING ENERGY... ✦", cw)];
            let pose = StagePose { fx_lines: &fx, fx_col: type_rgb(&ctype), msg: &msg, anim_tick: *tick, muted: bgm.is_muted(), ranked, streak, ..Default::default() };
            draw_stage(player, enemy, counts, &pose);
            push_logs(recent_log, full_log, msg);
            thread::sleep(Duration::from_millis(800));
            return;
        }
        atk.charging_move = None;
        if m.pp > 0 { m.pp -= 1; }
        if m.recharge_turn { atk.must_recharge = true; }
        m.clone()
    };

    let atk_name = if p_attacking { player.name.clone() } else { enemy.name.clone() };
    animate_attack(player, enemy, counts, &mv, &atk_name, p_attacking, bgm, ranked, streak, tick);

    if !mv.target_self && ((r % 100) as i32) >= mv.accuracy {
        let msg = format!("[{}] {} used {} — but it missed!", side, atk_name, mv.name);
        let fx = vec![center_str("MISS!", cw)];
        let pose = StagePose { fx_lines: &fx, fx_col: (180, 185, 195), msg: &msg, anim_tick: *tick, muted: bgm.is_muted(), ranked, streak, ..Default::default() };
        draw_stage(player, enemy, counts, &pose);
        push_logs(recent_log, full_log, msg);
        thread::sleep(Duration::from_millis(750));
        return;
    }

    if mv.is_status {
        let mut msg = format!("[{}] {} used {} ({})!", side, atk_name, mv.name, mv.mtype);
        if mv.healing > 0 {
            let heal = {
                let atk = if p_attacking { &mut *player } else { &mut *enemy };
                let h = (atk.max_hp * mv.healing / 100).max(20);
                atk.hp = (atk.hp + h).min(atk.max_hp);
                h
            };
            msg = format!("[{}] {} used {} -> +{} HP!", side, atk_name, mv.name, heal);
        } else if !mv.stat_changes.is_empty() {
            let note = if mv.target_self {
                let atk = if p_attacking { &mut *player } else { &mut *enemy };
                apply_stat_changes(atk, &mv.stat_changes)
            } else {
                let def = if p_attacking { &mut *enemy } else { &mut *player };
                apply_stat_changes(def, &mv.stat_changes)
            };
            if !note.is_empty() {
                msg = format!("[{}] {} used {} ({})!", side, atk_name, mv.name, note);
            }
        } else {
            let def = if p_attacking { &mut *enemy } else { &mut *player };
            if def.status == Status::None {
                match mv.ailment.as_str() {
                    "burn" => { def.status = Status::Burn; msg = format!("[{}] {} used {} -> {} burned!", side, atk_name, mv.name, def.name); }
                    "paralysis" => { def.status = Status::Paralyze; msg = format!("[{}] {} used {} -> {} paralyzed!", side, atk_name, mv.name, def.name); }
                    "sleep" => { def.status = Status::Sleep(((r % 3) + 1) as u8); msg = format!("[{}] {} used {} -> {} fell asleep!", side, atk_name, mv.name, def.name); }
                    "poison" => { def.status = Status::Poison(1); msg = format!("[{}] {} used {} -> {} poisoned!", side, atk_name, mv.name, def.name); }
                    "freeze" => { def.status = Status::Freeze; msg = format!("[{}] {} used {} -> {} frozen!", side, atk_name, mv.name, def.name); }
                    _ => {}
                }
            } else {
                msg = format!("[{}] {} used {} — {} already has a status!", side, atk_name, mv.name, def.name);
            }
        }
        let fx = vec![center_str("✦ STATUS EFFECT ✦", cw)];
        let pose = StagePose { fx_lines: &fx, fx_col: type_rgb(&mv.mtype), msg: &msg, sub_msg: &mv.desc, anim_tick: *tick, muted: bgm.is_muted(), ranked, streak, ..Default::default() };
        draw_stage(player, enemy, counts, &pose);
        push_logs(recent_log, full_log, msg);
        thread::sleep(Duration::from_millis(800));
        return;
    }

    let crit = (r % 16) == 0;
    let roll = 0.85 + ((r % 16) as f32 * 0.01);
    let (mut dmg, eff, ab_block) = if p_attacking {
        calc_move_damage(&mv, player, enemy, crit, roll)
    } else {
        calc_move_damage(&mv, enemy, player, crit, roll)
    };

    if let Some(ab) = ab_block {
        let def = if p_attacking { &mut *enemy } else { &mut *player };
        if ab == "Flash Fire" {
            def.flash_fire_boost = true;
        } else if ab == "Water Absorb" || ab == "Volt Absorb" {
            def.hp = (def.hp + def.max_hp / 4).min(def.max_hp);
        }
        let msg = format!("[{}] {}'s {} blocked {}!", side, def.name, ab, mv.name);
        let fx = vec![center_str("✦ ABILITY BLOCKED! ✦", cw)];
        let pose = StagePose { fx_lines: &fx, fx_col: (110, 235, 255), msg: &msg, anim_tick: *tick, muted: bgm.is_muted(), ranked, streak, ..Default::default() };
        draw_stage(player, enemy, counts, &pose);
        push_logs(recent_log, full_log, msg);
        thread::sleep(Duration::from_millis(800));
        return;
    }

    if eff == 0.0 {
        let msg = format!("[{}] {} used {} — no effect on {}!", side, atk_name, mv.name, if p_attacking { &enemy.name } else { &player.name });
        let fx = vec![center_str("NO EFFECT (0x)", cw)];
        let pose = StagePose { fx_lines: &fx, fx_col: (160, 165, 175), msg: &msg, anim_tick: *tick, muted: bgm.is_muted(), ranked, streak, ..Default::default() };
        draw_stage(player, enemy, counts, &pose);
        push_logs(recent_log, full_log, msg);
        thread::sleep(Duration::from_millis(750));
        return;
    }

    let mut clutch_note = "";
    {
        let def = if p_attacking { &mut *enemy } else { &mut *player };
        if dmg >= def.hp && def.hp == def.max_hp && ability_is(def, "sturdy") {
            dmg = def.hp - 1;
            clutch_note = " Held on with Sturdy!";
        }
    }

    if p_attacking {
        stats.total_dmg += dmg;
        if eff >= 1.9 { stats.super_hits += 1; }
        if crit { stats.crits += 1; }
    }

    let eff_txt = if eff >= 3.9 { " [4x MEGA!]" }
        else if eff >= 1.9 { " [2x SUPER!]" }
        else if eff <= 0.51 { " [0.5x RES]" }
        else { "" };
    let crit_txt = if crit { " CRIT!" } else { "" };
    let cat = if mv.is_physical { "Phys" } else { "Spec" };
    let hit_msg = format!(
        "[{}] {} used {} ({}/{} {}BP) -> -{} HP!{}{}{}",
        side, atk_name, mv.name, mv.mtype, cat, mv.power, dmg, crit_txt, eff_txt, clutch_note
    );

    animate_damage(player, enemy, counts, p_attacking, dmg, &hit_msg, &mv.desc, bgm, ranked, streak, tick);
    push_logs(recent_log, full_log, hit_msg);

    // Check Opponent Sitrus Berry trigger at <=50% HP
    if p_attacking && enemy.hp > 0 && enemy.sitrus_berry && enemy.hp * 2 <= enemy.max_hp {
        enemy.sitrus_berry = false;
        let heal = (enemy.max_hp / 4).max(25);
        enemy.hp = (enemy.hp + heal).min(enemy.max_hp);
        let bmsg = format!("[OPPONENT] {} ate its Sitrus Berry (+{} HP)!", enemy.name, heal);
        let fx = vec![center_str("✦ SITRUS BERRY HEAL! ✦", cw)];
        let pose = StagePose { fx_lines: &fx, fx_col: (90, 235, 115), msg: &bmsg, anim_tick: *tick, muted: bgm.is_muted(), ranked, streak, ..Default::default() };
        draw_stage(player, enemy, counts, &pose);
        push_logs(recent_log, full_log, bmsg);
        thread::sleep(Duration::from_millis(700));
    }

    // Drain / Recoil
    if mv.drain > 0 && dmg > 0 {
        let heal = (dmg * mv.drain / 100).max(1);
        let atk = if p_attacking { &mut *player } else { &mut *enemy };
        atk.hp = (atk.hp + heal).min(atk.max_hp);
    } else if mv.drain < 0 && dmg > 0 {
        let recoil = (dmg * (-mv.drain) / 100).max(1);
        let atk = if p_attacking { &mut *player } else { &mut *enemy };
        atk.hp = (atk.hp - recoil).max(0);
    }

    // Secondary ailment chance
    let def_alive = if p_attacking { enemy.hp > 0 } else { player.hp > 0 };
    if def_alive && mv.ailment != "none" && (mv.ailment_chance == 0 || ((r % 100) as i32) < mv.ailment_chance.max(15)) {
        let def = if p_attacking { &mut *enemy } else { &mut *player };
        if def.status == Status::None {
            match mv.ailment.as_str() {
                "burn" => def.status = Status::Burn,
                "paralysis" => def.status = Status::Paralyze,
                "poison" => def.status = Status::Poison(1),
                "freeze" => def.status = Status::Freeze,
                _ => {}
            }
        }
    }
}

// =========================================================================
// MAIN BATTLE LOOP
// =========================================================================

fn parse_team_arg(arg: &str) -> Vec<String> {
    if arg == "--box" {
        let names = load_box_names();
        if !names.is_empty() {
            return names.into_iter().take(6).collect();
        }
    }
    arg.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .take(6)
        .collect()
}

pub fn run_fight_mode(p1_spec: Option<&str>, p2_spec: Option<&str>, start_muted: bool) {
    let ranked_random = p2_spec.is_none();

    let p1_names = match p1_spec {
        Some(s) => parse_team_arg(s),
        None => vec![((rand_seed() % 1025) + 1).to_string()],
    };

    // Fair completely random opponent selection across all 1,025 Pokemon (mirror matches allowed)
    let p2_names = match p2_spec {
        Some(s) => parse_team_arg(s),
        None => (0..p1_names.len().max(1))
            .map(|i| {
                let s = rand_seed().wrapping_add((i as u32 + 1).wrapping_mul(2654435761));
                ((s % 1025) + 1).to_string()
            })
            .collect(),
    };

    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner:.yellow} Loading animated sprites, canon movesets & Grandmaster Opponent...")
            .unwrap(),
    );
    pb.enable_steady_tick(Duration::from_millis(80));

    let load_team = |names: Vec<String>, is_p: bool| -> Vec<Fighter> {
        thread::scope(|s| {
            let hs: Vec<_> = names
                .into_iter()
                .map(|n| s.spawn(move || fetch_pokemon(&n, false).map(|d| build_fighter(d, is_p))))
                .collect();
            hs.into_iter().filter_map(|h| h.join().ok().flatten()).collect()
        })
    };

    let (mut p_team, mut e_team) = thread::scope(|s| {
        let h1 = s.spawn(|| load_team(p1_names, true));
        let h2 = s.spawn(|| load_team(p2_names, false));
        (h1.join().unwrap_or_default(), h2.join().unwrap_or_default())
    });
    pb.finish_and_clear();

    if p_team.is_empty() || e_team.is_empty() {
        eprintln!("  {}", "Could not load teams. Try: pokefact --battle gengar".red().bold());
        return;
    }

    let _guard = ScreenGuard;
    let _ = enable_raw_mode();
    let mut out = io::stdout();
    let _ = write!(out, "\x1b[?1049h\x1b[?25l\x1b[2J");
    let _ = out.flush();

    let bgm = BgmPlayer::start(start_muted);
    let current_streak = load_streaks().current_streak;

    let (mut p_idx, mut e_idx) = (0usize, 0usize);
    let mut bag = Bag { potions: 1, heals: 1, tonics: 1, balls: 2 };
    let mut opp_potions: u8 = 1;
    let mut stats = MatchStats { turns: 1, total_dmg: 0, super_hits: 0, crits: 0, caught: 0 };
    let mut recent_log: Vec<String> = Vec::new();
    let mut full_log: Vec<String> = Vec::new();
    let mut tick: usize = 0;

    if ability_is(&p_team[p_idx], "intimidate") { e_team[e_idx].stages[0] = -1; }
    if ability_is(&e_team[e_idx], "intimidate") { p_team[p_idx].stages[0] = -1; }

    let mut status_msg = format!(
        "A challenger appears! {} vs OPPONENT's {}!",
        p_team[p_idx].name, e_team[e_idx].name
    );
    push_logs(&mut recent_log, &mut full_log, format!("Battle started: {} vs {}", p_team[p_idx].name, e_team[e_idx].name));

    loop {
        let p_alive = p_team.iter().filter(|f| f.hp > 0).count();
        let e_alive = e_team.iter().filter(|f| f.hp > 0).count();
        let counts = (p_alive, p_team.len(), e_alive, e_team.len());

        let (tw, _) = term_dims();
        let cw = tw.saturating_sub(7).clamp(28, 88);

        enum PlayerAction {
            Move(usize),
            Switch(usize),
            Potion,
            Heal,
            Tonic,
            Ball,
            Run,
        }

        // If player is mid-charge or recharging, skip menu automatically
        let action: PlayerAction = if let Some(ch_m) = p_team[p_idx].charging_move {
            PlayerAction::Move(ch_m)
        } else if p_team[p_idx].must_recharge {
            PlayerAction::Move(0)
        } else {
            let main_menu = vec![
                pad_right(
                    &format!(
                        "[1] FIGHT ({} moves)  [2] BAG ({} items)  [I] INSPECT",
                        p_team[p_idx].moves.len(),
                        bag.potions + bag.heals + bag.tonics + bag.balls
                    ),
                    cw,
                )
                .truecolor(235, 240, 250)
                .to_string(),
                pad_right(
                    &format!(
                        "[3] PARTY ({}/{})      [L] FULL LOG       [4] RUN",
                        p_alive, p_team.len()
                    ),
                    cw,
                )
                .truecolor(195, 205, 220)
                .to_string(),
            ];

            let Some(key) = wait_key_animated(
                &mut p_team[p_idx],
                &mut e_team[e_idx],
                counts,
                &status_msg,
                "",
                &recent_log,
                &main_menu,
                &bgm,
                ranked_random,
                current_streak,
                &mut tick,
            ) else {
                break;
            };

            let picked = match key {
                KeyCode::Char('4') | KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => {
                    Some(PlayerAction::Run)
                }
                KeyCode::Char('i') | KeyCode::Char('I') => {
                    let (x4, x2, h, q, imm) = type_matchups(&e_team[e_idx].types);
                    let mut i_lines = vec![
                        pad_right(&format!("OPPONENT: {} | Types: {}", e_team[e_idx].name, e_team[e_idx].types.join(" / ")), cw)
                            .truecolor(255, 215, 70).bold().to_string(),
                        pad_right(&format!("Ability: {} | Sitrus Berry: {}", e_team[e_idx].ability, if e_team[e_idx].sitrus_berry { "Yes" } else { "Used" }), cw)
                            .truecolor(180, 220, 255).to_string(),
                    ];
                    if !x4.is_empty() { i_lines.push(pad_right(&format!("4x Weak: {}", x4.join(", ")), cw).truecolor(255, 80, 80).to_string()); }
                    if !x2.is_empty() { i_lines.push(pad_right(&format!("2x Weak: {}", x2.join(", ")), cw).truecolor(255, 150, 85).to_string()); }
                    if !imm.is_empty() { i_lines.push(pad_right(&format!("Immune:  {}", imm.join(", ")), cw).truecolor(110, 235, 255).to_string()); }
                    let mut res = q;
                    res.extend(h);
                    if !res.is_empty() { i_lines.push(pad_right(&format!("Resists: {}", res.join(", ")), cw).truecolor(95, 230, 130).to_string()); }
                    i_lines.push(pad_right("Press any key to return...", cw).dimmed().to_string());
                    let _ = wait_key_animated(
                        &mut p_team[p_idx], &mut e_team[e_idx], counts,
                        "OPPONENT SCOUTING REPORT", "", &[], &i_lines,
                        &bgm, ranked_random, current_streak, &mut tick,
                    );
                    None
                }
                KeyCode::Char('l') | KeyCode::Char('L') => {
                    let mut l_lines: Vec<String> = full_log
                        .iter()
                        .rev()
                        .take(7)
                        .rev()
                        .map(|s| pad_right(s, cw).truecolor(200, 215, 230).to_string())
                        .collect();
                    l_lines.push(pad_right("Press any key to return...", cw).dimmed().to_string());
                    let _ = wait_key_animated(
                        &mut p_team[p_idx], &mut e_team[e_idx], counts,
                        &format!("BATTLE LOG (Last {} events)", full_log.len().min(7)), "", &[], &l_lines,
                        &bgm, ranked_random, current_streak, &mut tick,
                    );
                    None
                }
                KeyCode::Char('1') => {
                    let mut page = 0usize;
                    let per_page = 4usize;
                    let mut mv_pick = None;
                    loop {
                        let (tw_m, _) = term_dims();
                        let cw_m = tw_m.saturating_sub(7).clamp(28, 88);
                        let (total_pages, start, end, m_lines) = {
                            let moves = &p_team[p_idx].moves;
                            let total_pages = ((moves.len() + per_page - 1) / per_page).max(1);
                            let start = page * per_page;
                            let end = (start + per_page).min(moves.len());
                            let mut lines = Vec::new();
                            let mults = defensive_multipliers(&e_team[e_idx].types);
                            for (slot, mv) in moves[start..end].iter().enumerate() {
                                let cat = if mv.is_status { "Stat" } else if mv.is_physical { "Phys" } else { "Spec" };
                                let eff_b = if mv.is_status {
                                    ""
                                } else {
                                    let m = mults.iter().find(|(k, _)| k.eq_ignore_ascii_case(&mv.mtype)).map(|(_, &v)| v).unwrap_or(1.0);
                                    if m >= 3.9 { " [4x]" } else if m >= 1.9 { " [2x]" } else if m == 0.0 { " [0x]" } else if m <= 0.51 { " [.5x]" } else { "" }
                                };
                                let stab = if !mv.is_status && p_team[p_idx].types.iter().any(|t| t.eq_ignore_ascii_case(&mv.mtype)) { "*" } else { "" };
                                let head = format!(
                                    "[{}] {}{} ({}/{} {}BP {}/{}PP){}",
                                    slot + 1, mv.name, stab, mv.mtype, cat, mv.power, mv.pp, mv.max_pp, eff_b
                                );
                                let mc = type_rgb(&mv.mtype);
                                lines.push(pad_right(&head, cw_m).truecolor(mc.0, mc.1, mc.2).bold().to_string());
                                lines.push(pad_right(&format!("    [{}] {}", mv.source, mv.desc), cw_m).dimmed().to_string());
                            }
                            lines.push(
                                pad_right(
                                    &format!("Page {}/{} | [1-{}] Attack | [N] Next | [P] Prev | [B] Back", page + 1, total_pages, end - start),
                                    cw_m,
                                )
                                .yellow()
                                .to_string(),
                            );
                            (total_pages, start, end, lines)
                        };

                        let Some(mk) = wait_key_animated(
                            &mut p_team[p_idx], &mut e_team[e_idx], counts,
                            "Choose an attack (* = STAB bonus):", "", &[], &m_lines,
                            &bgm, ranked_random, current_streak, &mut tick,
                        ) else {
                            break;
                        };
                        match mk {
                            KeyCode::Char('b') | KeyCode::Char('B') | KeyCode::Esc => break,
                            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Right => page = (page + 1) % total_pages,
                            KeyCode::Char('p') | KeyCode::Char('P') | KeyCode::Left => page = (page + total_pages - 1) % total_pages,
                            KeyCode::Char(c) if ('1'..='4').contains(&c) => {
                                let n = (c as u8 - b'1') as usize;
                                if start + n < end && p_team[p_idx].moves[start + n].pp > 0 {
                                    mv_pick = Some(PlayerAction::Move(start + n));
                                    break;
                                }
                            }
                            _ => {}
                        }
                    }
                    mv_pick
                }
                KeyCode::Char('2') => {
                    let b_lines = vec![
                        pad_right(&format!("[1] Hyper Potion (x{}) — Restores 120 HP", bag.potions), cw).truecolor(90, 235, 115).to_string(),
                        pad_right(&format!("[2] Full Heal    (x{}) — Cures status condition", bag.heals), cw).truecolor(110, 235, 255).to_string(),
                        pad_right(&format!("[3] Power Tonic  (x{}) — +1 Atk & Sp.Atk stage", bag.tonics), cw).truecolor(255, 215, 70).to_string(),
                        pad_right(&format!("[4] Ultra Ball   (x{}) — Catch Opponent into PC Box", bag.balls), cw).truecolor(255, 145, 95).to_string(),
                        pad_right("[B] Back", cw).dimmed().to_string(),
                    ];
                    match wait_key_animated(
                        &mut p_team[p_idx], &mut e_team[e_idx], counts,
                        "Select an item from your Bag:", "", &[], &b_lines,
                        &bgm, ranked_random, current_streak, &mut tick,
                    ) {
                        Some(KeyCode::Char('1')) if bag.potions > 0 => Some(PlayerAction::Potion),
                        Some(KeyCode::Char('2')) if bag.heals > 0 => Some(PlayerAction::Heal),
                        Some(KeyCode::Char('3')) if bag.tonics > 0 => Some(PlayerAction::Tonic),
                        Some(KeyCode::Char('4')) if bag.balls > 0 => Some(PlayerAction::Ball),
                        _ => None,
                    }
                }
                KeyCode::Char('3') => {
                    let mut p_lines = Vec::new();
                    for (i, f) in p_team.iter().enumerate() {
                        let tag = if i == p_idx { " [ACTIVE]" } else if f.hp == 0 { " [FAINTED]" } else { "" };
                        p_lines.push(pad_right(&format!("[{}] {} — {}/{} HP{}{}", i + 1, f.name, f.hp, f.max_hp, f.status.tag(), tag), cw));
                    }
                    p_lines.push(pad_right("[B] Back", cw).dimmed().to_string());
                    match wait_key_animated(
                        &mut p_team[p_idx], &mut e_team[e_idx], counts,
                        "Switch to which Pokemon?", "", &[], &p_lines,
                        &bgm, ranked_random, current_streak, &mut tick,
                    ) {
                        Some(KeyCode::Char(c)) if ('1'..='6').contains(&c) => {
                            let i = (c as u8 - b'1') as usize;
                            if i < p_team.len() && i != p_idx && p_team[i].hp > 0 {
                                Some(PlayerAction::Switch(i))
                            } else {
                                None
                            }
                        }
                        _ => None,
                    }
                }
                _ => None,
            };

            let Some(a) = picked else { continue; };
            a
        };

        if matches!(action, PlayerAction::Run) {
            if ranked_random && stats.turns > 1 {
                let rec = BattleRecord {
                    date: Local::now().format("%Y-%m-%d %H:%M").to_string(),
                    player: p_team[0].name.clone(),
                    opponent: e_team[0].name.clone(),
                    result: "FLED".to_string(),
                    turns: stats.turns,
                    dmg: stats.total_dmg,
                    hp_pct: (p_team[p_idx].hp * 100 / p_team[p_idx].max_hp.max(1)),
                    ranked: true,
                };
                record_battle_outcome(rec);
            }
            let flee = StagePose { msg: "You fled from the arena!", anim_tick: tick, muted: bgm.is_muted(), ranked: ranked_random, streak: current_streak, ..Default::default() };
            draw_stage(&mut p_team[p_idx], &mut e_team[e_idx], counts, &flee);
            thread::sleep(Duration::from_millis(600));
            break;
        }

        let opp_decision = choose_grandmaster_action(&e_team, e_idx, &p_team[p_idx], opp_potions);

        // Execute Opponent pre-turn actions (Switch or Potion) before player move if applicable
        let mut opp_acted = false;
        let opp_move_idx = match opp_decision {
            OppDecision::Switch(new_e) => {
                e_team[e_idx].stages = [0; 5];
                e_team[e_idx].charging_move = None;
                e_idx = new_e;
                if ability_is(&e_team[e_idx], "intimidate") {
                    p_team[p_idx].stages[0] = (p_team[p_idx].stages[0] - 1).clamp(-6, 6);
                }
                let smsg = format!("[OPPONENT] Switched to {}!", e_team[e_idx].name);
                let pose = StagePose { msg: &smsg, e_hop: 1, anim_tick: tick, muted: bgm.is_muted(), ranked: ranked_random, streak: current_streak, ..Default::default() };
                draw_stage(&mut p_team[p_idx], &mut e_team[e_idx], counts, &pose);
                push_logs(&mut recent_log, &mut full_log, smsg);
                thread::sleep(Duration::from_millis(650));
                opp_acted = true;
                0
            }
            OppDecision::HealPotion => {
                opp_potions -= 1;
                e_team[e_idx].hp = (e_team[e_idx].hp + 120).min(e_team[e_idx].max_hp);
                let hmsg = format!("[OPPONENT] Used Hyper Potion on {} (+120 HP)!", e_team[e_idx].name);
                let fx = vec![center_str("✦ OPPONENT HEALED +120 HP! ✦", cw)];
                let pose = StagePose { fx_lines: &fx, fx_col: (90, 235, 115), msg: &hmsg, e_hop: 1, anim_tick: tick, muted: bgm.is_muted(), ranked: ranked_random, streak: current_streak, ..Default::default() };
                draw_stage(&mut p_team[p_idx], &mut e_team[e_idx], counts, &pose);
                push_logs(&mut recent_log, &mut full_log, hmsg);
                thread::sleep(Duration::from_millis(750));
                opp_acted = true;
                0
            }
            OppDecision::Move(m) => m,
        };

        match action {
            PlayerAction::Switch(new_i) => {
                p_team[p_idx].stages = [0; 5];
                p_team[p_idx].charging_move = None;
                p_idx = new_i;
                if ability_is(&p_team[p_idx], "intimidate") { e_team[e_idx].stages[0] = (e_team[e_idx].stages[0] - 1).clamp(-6, 6); }
                let sw_msg = format!("[YOU] Go, {}!", p_team[p_idx].name);
                let pose = StagePose { msg: &sw_msg, p_hop: 1, anim_tick: tick, muted: bgm.is_muted(), ranked: ranked_random, streak: current_streak, ..Default::default() };
                draw_stage(&mut p_team[p_idx], &mut e_team[e_idx], counts, &pose);
                push_logs(&mut recent_log, &mut full_log, sw_msg);
                thread::sleep(Duration::from_millis(550));
                if !opp_acted {
                    resolve_turn_action(&mut p_team[p_idx], &mut e_team[e_idx], false, opp_move_idx, counts, &mut stats, &mut recent_log, &mut full_log, &bgm, ranked_random, current_streak, &mut tick);
                }
            }
            PlayerAction::Potion => {
                bag.potions -= 1;
                p_team[p_idx].hp = (p_team[p_idx].hp + 120).min(p_team[p_idx].max_hp);
                let fx = vec![center_str("✦ +120 HP RESTORED! ✦", cw)];
                let msg = format!("[YOU] Used Hyper Potion on {}!", p_team[p_idx].name);
                let pose = StagePose { fx_lines: &fx, fx_col: (90, 235, 115), msg: &msg, p_hop: 1, anim_tick: tick, muted: bgm.is_muted(), ranked: ranked_random, streak: current_streak, ..Default::default() };
                draw_stage(&mut p_team[p_idx], &mut e_team[e_idx], counts, &pose);
                push_logs(&mut recent_log, &mut full_log, msg);
                thread::sleep(Duration::from_millis(700));
                if !opp_acted {
                    resolve_turn_action(&mut p_team[p_idx], &mut e_team[e_idx], false, opp_move_idx, counts, &mut stats, &mut recent_log, &mut full_log, &bgm, ranked_random, current_streak, &mut tick);
                }
            }
            PlayerAction::Heal => {
                bag.heals -= 1;
                p_team[p_idx].status = Status::None;
                let fx = vec![center_str("✦ STATUS CURED! ✦", cw)];
                let msg = format!("[YOU] {}'s status was cured!", p_team[p_idx].name);
                let pose = StagePose { fx_lines: &fx, fx_col: (110, 235, 255), msg: &msg, p_hop: 1, anim_tick: tick, muted: bgm.is_muted(), ranked: ranked_random, streak: current_streak, ..Default::default() };
                draw_stage(&mut p_team[p_idx], &mut e_team[e_idx], counts, &pose);
                push_logs(&mut recent_log, &mut full_log, msg);
                thread::sleep(Duration::from_millis(700));
                if !opp_acted {
                    resolve_turn_action(&mut p_team[p_idx], &mut e_team[e_idx], false, opp_move_idx, counts, &mut stats, &mut recent_log, &mut full_log, &bgm, ranked_random, current_streak, &mut tick);
                }
            }
            PlayerAction::Tonic => {
                bag.tonics -= 1;
                p_team[p_idx].stages[0] = (p_team[p_idx].stages[0] + 1).clamp(-6, 6);
                p_team[p_idx].stages[2] = (p_team[p_idx].stages[2] + 1).clamp(-6, 6);
                let fx = vec![center_str("✦ POWER UP! (+1 ATK/SPA) ✦", cw)];
                let msg = format!("[YOU] Used Power Tonic (+1 Atk & Sp.Atk)!");
                let pose = StagePose { fx_lines: &fx, fx_col: (255, 215, 70), msg: &msg, p_hop: 1, anim_tick: tick, muted: bgm.is_muted(), ranked: ranked_random, streak: current_streak, ..Default::default() };
                draw_stage(&mut p_team[p_idx], &mut e_team[e_idx], counts, &pose);
                push_logs(&mut recent_log, &mut full_log, msg);
                thread::sleep(Duration::from_millis(700));
                if !opp_acted {
                    resolve_turn_action(&mut p_team[p_idx], &mut e_team[e_idx], false, opp_move_idx, counts, &mut stats, &mut recent_log, &mut full_log, &bgm, ranked_random, current_streak, &mut tick);
                }
            }
            PlayerAction::Ball => {
                bag.balls -= 1;
                let hp_factor = 1.0 - (e_team[e_idx].hp as f32 / e_team[e_idx].max_hp as f32);
                let st_bonus = if e_team[e_idx].status != Status::None { 0.20 } else { 0.0 };
                let chance = ((0.14 + hp_factor * 0.62 + st_bonus) * 100.0).clamp(12.0, 92.0) as u32;
                let caught = (rand_seed() % 100) < chance;
                animate_pokeball(&mut p_team[p_idx], &mut e_team[e_idx], counts, caught, &bgm, ranked_random, current_streak, &mut tick);
                if caught {
                    stats.caught += 1;
                    save_to_box_quiet(&e_team[e_idx].data);
                    e_team[e_idx].hp = 0;
                    push_logs(&mut recent_log, &mut full_log, format!("[YOU] Caught {}!", e_team[e_idx].name));
                } else {
                    push_logs(&mut recent_log, &mut full_log, format!("[YOU] {} broke free!", e_team[e_idx].name));
                    if !opp_acted {
                        resolve_turn_action(&mut p_team[p_idx], &mut e_team[e_idx], false, opp_move_idx, counts, &mut stats, &mut recent_log, &mut full_log, &bgm, ranked_random, current_streak, &mut tick);
                    }
                }
            }
            PlayerAction::Move(p_mv) => {
                if opp_acted {
                    resolve_turn_action(&mut p_team[p_idx], &mut e_team[e_idx], true, p_mv, counts, &mut stats, &mut recent_log, &mut full_log, &bgm, ranked_random, current_streak, &mut tick);
                } else {
                    let p_prio = p_team[p_idx].moves[p_mv].priority;
                    let e_prio = e_team[e_idx].moves[opp_move_idx].priority;
                    let p_first = if p_prio != e_prio {
                        p_prio > e_prio
                    } else {
                        effective_speed(&p_team[p_idx]) >= effective_speed(&e_team[e_idx])
                    };

                    if p_first {
                        resolve_turn_action(&mut p_team[p_idx], &mut e_team[e_idx], true, p_mv, counts, &mut stats, &mut recent_log, &mut full_log, &bgm, ranked_random, current_streak, &mut tick);
                        if e_team[e_idx].hp > 0 && p_team[p_idx].hp > 0 {
                            resolve_turn_action(&mut p_team[p_idx], &mut e_team[e_idx], false, opp_move_idx, counts, &mut stats, &mut recent_log, &mut full_log, &bgm, ranked_random, current_streak, &mut tick);
                        }
                    } else {
                        resolve_turn_action(&mut p_team[p_idx], &mut e_team[e_idx], false, opp_move_idx, counts, &mut stats, &mut recent_log, &mut full_log, &bgm, ranked_random, current_streak, &mut tick);
                        if p_team[p_idx].hp > 0 && e_team[e_idx].hp > 0 {
                            resolve_turn_action(&mut p_team[p_idx], &mut e_team[e_idx], true, p_mv, counts, &mut stats, &mut recent_log, &mut full_log, &bgm, ranked_random, current_streak, &mut tick);
                        }
                    }
                }
            }
            PlayerAction::Run => unreachable!(),
        }

        // End-of-turn Burn / Poison chip
        for is_p in [true, false] {
            let f = if is_p { &mut p_team[p_idx] } else { &mut e_team[e_idx] };
            if f.hp == 0 { continue; }
            match f.status {
                Status::Burn => {
                    let chip = (f.max_hp / 16).max(1);
                    f.hp = (f.hp - chip).max(0);
                }
                Status::Poison(n) => {
                    let chip = (f.max_hp * (n as i32) / 16).max(1);
                    f.hp = (f.hp - chip).max(0);
                    f.status = Status::Poison(n.saturating_add(1).min(6));
                }
                _ => {}
            }
        }

        // Check KO & Victory
        let mut opp_switched = false;
        if e_team[e_idx].hp == 0 {
            animate_ko(&mut p_team[p_idx], &mut e_team[e_idx], counts, true, &bgm, ranked_random, current_streak, &mut tick);
            if let Some(next_e) = e_team.iter().position(|f| f.hp > 0) {
                e_idx = next_e;
                if ability_is(&e_team[e_idx], "intimidate") { p_team[p_idx].stages[0] = (p_team[p_idx].stages[0] - 1).clamp(-6, 6); }
                status_msg = format!("OPPONENT sent out {}!", e_team[e_idx].name);
                opp_switched = true;
            } else {
                let hp_pct = (p_team[p_idx].hp * 100 / p_team[p_idx].max_hp.max(1)).clamp(0, 100);
                let rec = BattleRecord {
                    date: Local::now().format("%Y-%m-%d %H:%M").to_string(),
                    player: p_team[0].name.clone(),
                    opponent: e_team[0].name.clone(),
                    result: if stats.caught > 0 { "CAUGHT".into() } else { "WIN".into() },
                    turns: stats.turns,
                    dmg: stats.total_dmg,
                    hp_pct,
                    ranked: ranked_random,
                };
                let (new_s, best_s) = record_battle_outcome(rec);
                animate_victory(&mut p_team[p_idx], &mut e_team[e_idx], (p_alive, counts.1, 0, counts.3), true, &stats, &bgm, ranked_random, new_s, best_s, &mut tick);
                break;
            }
        }

        if p_team[p_idx].hp == 0 {
            animate_ko(&mut p_team[p_idx], &mut e_team[e_idx], counts, false, &bgm, ranked_random, current_streak, &mut tick);
            if let Some(next_p) = p_team.iter().position(|f| f.hp > 0) {
                p_idx = next_p;
                if ability_is(&p_team[p_idx], "intimidate") { e_team[e_idx].stages[0] = (e_team[e_idx].stages[0] - 1).clamp(-6, 6); }
                status_msg = format!("You sent out {}!", p_team[p_idx].name);
            } else {
                let rec = BattleRecord {
                    date: Local::now().format("%Y-%m-%d %H:%M").to_string(),
                    player: p_team[0].name.clone(),
                    opponent: e_team[0].name.clone(),
                    result: "LOSS".into(),
                    turns: stats.turns,
                    dmg: stats.total_dmg,
                    hp_pct: 0,
                    ranked: ranked_random,
                };
                let (new_s, best_s) = record_battle_outcome(rec);
                animate_victory(&mut p_team[p_idx], &mut e_team[e_idx], (0, counts.1, e_alive, counts.3), false, &stats, &bgm, ranked_random, new_s, best_s, &mut tick);
                break;
            }
        } else if !opp_switched {
            status_msg = format!("Turn {} — What will {} do?", stats.turns + 1, p_team[p_idx].name);
        }

        stats.turns += 1;
    }
}
