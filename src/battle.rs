use std::collections::HashSet;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use colored::*;
use indicatif::{ProgressBar, ProgressStyle};
use serde_json::Value;

use crate::graphics::render_sprite_lines;
use crate::modes::{catch_pokemon, load_box_names};
use crate::{
    center_str, defensive_multipliers, fetch_bytes, fetch_pokemon, get_stat, hp_bar, pad_right,
    rand_seed, signature_move_for_type, spawn_play_cry, type_rgb, wrap_text, Capitalize, Rgb,
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
    bond_used: bool,
    front_bytes: Option<Vec<u8>>,
    back_bytes: Option<Vec<u8>>,
}

struct Bag {
    potions: u8,
    heals: u8,
    tonics: u8,
    balls: u8,
}

struct ScreenGuard;

impl Drop for ScreenGuard {
    fn drop(&mut self) {
        let mut out = io::stdout();
        let _ = write!(out, "\x1b[?25h\x1b[?1049l");
        let _ = out.flush();
    }
}

fn term_dims() -> (usize, usize) {
    terminal_size::terminal_size()
        .map(|(terminal_size::Width(w), terminal_size::Height(h))| (w as usize, h as usize))
        .unwrap_or((90, 28))
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

    if desc.is_empty() {
        desc = if dclass == "status" {
            format!("{} support move.", mtype)
        } else {
            format!("{} {} attack ({} power).", if dclass == "physical" { "Physical" } else { "Special" }, mtype, power)
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
        desc,
        source: source.to_string(),
    })
}

fn build_canon_movepool(data: &Value, types: &[String], phys: bool) -> Vec<BattleMove> {
    let mut seen = HashSet::new();
    let mut candidates: Vec<(String, &'static str)> = Vec::new();

    let mut push_cand = |raw: &str, src: &'static str| {
        let slug = raw.trim().to_lowercase().replace(' ', "-").replace('\'', "");
        if !slug.is_empty() && seen.insert(slug.clone()) {
            candidates.push((slug, src));
        }
    };

    if let Some(arr) = data["anime_moves"].as_array() {
        for m in arr.iter().take(4) {
            if let Some(n) = m.as_str() { push_cand(n, "ANIME"); }
        }
    }
    if let Some(arr) = data["manga_moves"].as_array() {
        for m in arr.iter().take(3) {
            if let Some(n) = m.as_str() { push_cand(n, "MANGA"); }
        }
    }
    if let Some(arr) = data["level_moves"].as_array() {
        for m in arr.iter().rev().take(8) {
            if let Some(n) = m[1].as_str() { push_cand(n, "GAME"); }
        }
    }
    if let Some(arr) = data["tm_moves"].as_array() {
        for m in arr.iter().take(5) {
            if let Some(n) = m.as_str() { push_cand(n, "TM"); }
        }
    }

    let fetched: Vec<BattleMove> = thread::scope(|s| {
        let handles: Vec<_> = candidates
            .into_iter()
            .take(14)
            .map(|(slug, src)| s.spawn(move || fetch_move_with_source(&slug, src)))
            .collect();
        handles.into_iter().filter_map(|h| h.join().ok().flatten()).collect()
    });

    let mut dmg: Vec<BattleMove> = fetched.iter().filter(|m| !m.is_status && m.power > 0).cloned().collect();
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

    dmg.sort_by(|a, b| b.power.cmp(&a.power));
    let mut deck: Vec<BattleMove> = dmg.into_iter().take(8).collect();
    deck.extend(sup.drain(..sup.len().min(4)));

    if deck.iter().all(|m| m.is_status) {
        let t = types.first().cloned().unwrap_or_else(|| "Normal".to_string());
        deck.insert(0, BattleMove {
            name: signature_move_for_type(&t, phys).to_string(),
            mtype: t.clone(),
            power: 85,
            accuracy: 100,
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
            desc: format!("Signature {} attack fueled by trainer bond.", t),
            source: "CANON".to_string(),
        });
    }
    if !deck.iter().any(|m| m.is_status) {
        deck.push(BattleMove {
            name: "Focus Spirit".to_string(),
            mtype: types.first().cloned().unwrap_or_else(|| "Normal".to_string()),
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
            desc: "Channels inner focus to boost Attack and Sp. Atk by +1 stage.",
            source: "ANIME".to_string(),
        });
    }

    deck
}

fn build_fighter(data: Value) -> Fighter {
    let id = data["id"].as_i64().unwrap_or(0);
    let name = data["name"].as_str().unwrap_or("UNKNOWN").to_string();
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
    let scaled = |k: &str| -> i32 { ((get_stat(&stats, k) as f32) * sc).round().max(25.0) as i32 };

    let hp_base = scaled("HP");
    let atk_base = scaled("Attack");
    let def_base = scaled("Defense");
    let spa_base = scaled("Sp. Atk");
    let spd_base = scaled("Sp. Def");
    let spe_base = scaled("Speed");

    let max_hp = hp_base * 2 + 115;
    let phys = atk_base >= spa_base;
    let front_url = data["normal_sprite"].as_str().unwrap_or("").to_string();
    let back_url = format!("https://raw.githubusercontent.com/PokeAPI/sprites/master/sprites/pokemon/back/{}.png", id);

    let (moves, front_bytes, back_bytes) = thread::scope(|s| {
        let hm = s.spawn(|| build_canon_movepool(&data, &types, phys));
        let hf = s.spawn(|| fetch_bytes(&front_url));
        let hb = s.spawn(|| fetch_bytes(&back_url));
        (
            hm.join().unwrap_or_default(),
            hf.join().ok().flatten(),
            hb.join().ok().flatten(),
        )
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
        bond_used: false,
        front_bytes,
        back_bytes,
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
    let mut off_val = (if mv.is_physical { atk.atk } else { atk.spa } as f32) * stage_mult(if crit && atk_stage < 0 { 0 } else { atk_stage });
    if mv.is_physical && atk.status == Status::Burn {
        off_val *= 0.5;
    }
    let def_val = ((if mv.is_physical { def.def } else { def.spd } as f32) * stage_mult(if crit && def_stage > 0 { 0 } else { def_stage })).max(1.0);

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
    player: &Fighter,
    enemy: &Fighter,
    p_alive: usize,
    p_total: usize,
    e_alive: usize,
    e_total: usize,
    fx_line: Option<(&str, Rgb)>,
    msg: &str,
    menu_lines: &[String],
    hide_p: bool,
    hide_e: bool,
    p_shift: usize,
    e_drop: usize,
    p_drop: usize,
) {
    let (tw, th) = term_dims();
    let box_w = tw.saturating_sub(2).clamp(32, 96);
    let inner = box_w - 2;
    let gold = (255, 215, 70);
    let pc = type_rgb(player.types.first().map(|s| s.as_str()).unwrap_or("cyan"));
    let ec = type_rgb(enemy.types.first().map(|s| s.as_str()).unwrap_or("magenta"));
    let bv = "│".truecolor(gold.0, gold.1, gold.2).to_string();

    let mut buf = String::with_capacity(8192);
    buf.push_str("\x1b[H");
    buf.push_str(&format!(
        " {}\x1b[K\n",
        format!("╭{}╮", "─".repeat(inner)).truecolor(gold.0, gold.1, gold.2)
    ));

    let e_head = format!(
        "[CPU {}/{}] {} Lv.50{} ({})",
        e_alive, e_total, enemy.name, enemy.status.tag(), enemy.ability
    );
    let e_hp_w = if inner >= 52 { 16 } else { 10 };
    let e_hp_txt = format!("HP {} {}/{}", hp_bar(enemy.hp, enemy.max_hp, e_hp_w), enemy.hp, enemy.max_hp);
    let e_st = format_stages(&enemy.stages);

    let p_head = format!(
        "[YOU {}/{}] {} Lv.50{} ({})",
        p_alive, p_total, player.name, player.status.tag(), player.ability
    );
    let p_hp_txt = format!("HP {} {}/{}", hp_bar(player.hp, player.max_hp, e_hp_w), player.hp, player.max_hp);
    let p_st = format_stages(&player.stages);

    buf.push_str(&format!(
        " {} {} {}\x1b[K\n",
        bv,
        pad_right(&e_head, inner - 2).truecolor(ec.0, ec.1, ec.2).bold(),
        bv
    ));
    let e_sub = if e_st.is_empty() { e_hp_txt.clone() } else { format!("{}  {}", e_hp_txt, e_st) };
    let e_plain_len = format!("HP [{}] {}/{}", " ".repeat(e_hp_w), enemy.hp, enemy.max_hp).len()
        + if e_st.is_empty() { 0 } else { 2 + e_st.len() };
    let e_pad = (inner - 2).saturating_sub(e_plain_len);
    buf.push_str(&format!(" {} {}{} {}\x1b[K\n", bv, e_sub, " ".repeat(e_pad), bv));

    let show_sprites = th >= 26 && inner >= 40;
    if show_sprites {
        let sp_w = ((inner / 2).saturating_sub(4) as u32).clamp(16, 30);
        let (pw, p_lines) = if hide_p {
            (sp_w as usize, vec![])
        } else {
            player
                .back_bytes
                .as_ref()
                .or(player.front_bytes.as_ref())
                .map(|b| render_sprite_lines(b, sp_w, false))
                .unwrap_or((10, vec![]))
        };
        let (ew, e_lines) = if hide_e {
            (sp_w as usize, vec![])
        } else {
            enemy
                .front_bytes
                .as_ref()
                .map(|b| render_sprite_lines(b, sp_w, false))
                .unwrap_or((10, vec![]))
        };

        let max_sp_rows = (th.saturating_sub(17 + menu_lines.len())).clamp(6, 14);
        let p_show: Vec<String> = p_lines.into_iter().take(max_sp_rows).collect();
        let e_show: Vec<String> = e_lines.into_iter().take(max_sp_rows).collect();
        let arena_h = p_show.len().max(e_show.len()).max(5);

        for r in 0..arena_h {
            let p_idx = r.checked_sub(arena_h.saturating_sub(p_show.len()) + p_drop);
            let e_idx = r.checked_sub(e_drop);
            let left_sp = p_idx.and_then(|i| p_show.get(i)).cloned().unwrap_or_else(|| " ".repeat(pw));
            let right_sp = e_idx.and_then(|i| e_show.get(i)).cloned().unwrap_or_else(|| " ".repeat(ew));
            let lp = (2 + p_shift).min(inner.saturating_sub(pw + ew + 4));
            let gap = (inner - 2).saturating_sub(lp + pw + ew + 2);
            buf.push_str(&format!(
                " {} {}{}{}{}{} {}\x1b[K\n",
                bv,
                " ".repeat(lp),
                left_sp,
                " ".repeat(gap),
                right_sp,
                " ".repeat((inner - 2).saturating_sub(lp + pw + gap + ew)),
                bv
            ));
        }
    }

    if let Some((fx, col)) = fx_line {
        let centered = center_str(fx, inner - 2);
        buf.push_str(&format!(
            " {} {} {}\x1b[K\n",
            bv,
            centered.truecolor(col.0, col.1, col.2).bold(),
            bv
        ));
    } else {
        buf.push_str(&format!(" {} {} {}\x1b[K\n", bv, " ".repeat(inner - 2), bv));
    }

    buf.push_str(&format!(
        " {} {} {}\x1b[K\n",
        bv,
        pad_right(&p_head, inner - 2).truecolor(pc.0, pc.1, pc.2).bold(),
        bv
    ));
    let p_sub = if p_st.is_empty() { p_hp_txt.clone() } else { format!("{}  {}", p_hp_txt, p_st) };
    let p_plain_len = format!("HP [{}] {}/{}", " ".repeat(e_hp_w), player.hp, player.max_hp).len()
        + if p_st.is_empty() { 0 } else { 2 + p_st.len() };
    let p_pad = (inner - 2).saturating_sub(p_plain_len);
    buf.push_str(&format!(" {} {}{} {}\x1b[K\n", bv, p_sub, " ".repeat(p_pad), bv));

    buf.push_str(&format!(
        " {}\x1b[K\n",
        format!("├{}┤", "─".repeat(inner)).truecolor(gold.0, gold.1, gold.2)
    ));

    for line in wrap_text(msg, inner - 2).into_iter().take(2) {
        buf.push_str(&format!(
            " {} {} {}\x1b[K\n",
            bv,
            pad_right(&line, inner - 2).truecolor(235, 240, 248).bold(),
            bv
        ));
    }

    if !menu_lines.is_empty() {
        buf.push_str(&format!(
            " {}\x1b[K\n",
            format!("├{}┤", "─".repeat(inner)).truecolor(gold.0, gold.1, gold.2)
        ));
        for ml in menu_lines {
            buf.push_str(&format!(" {} {}\x1b[K\n", bv, ml));
        }
    }

    buf.push_str(&format!(
        " {}\x1b[K\n\x1b[J",
        format!("╰{}╯", "─".repeat(inner)).truecolor(gold.0, gold.1, gold.2)
    ));

    let mut out = io::stdout();
    let _ = out.write_all(buf.as_bytes());
    let _ = out.flush();
}

fn attack_fx_frames(mtype: &str, is_status: bool) -> Vec<&'static str> {
    if is_status {
        return vec!["✦  ·  ✦", "·  ✦  ·", "✦ ✧ ✦ ✧ ✦", "✧  ✦  ✧", "·  ✦  ·"];
    }
    match mtype.to_lowercase().as_str() {
        "fire" => vec!["▲*", "▲▲**", "  ▲▲▲***", "    ▲▲▲***🔥", "      ***▲▲▲", "        ✦BOOM✦"],
        "water" => vec!["≈~", "≈≈~~", "  ≈≈≈~~~", "    ≈≈≈~~~●", "      ~~~≈≈≈", "        ✦SPLASH✦"],
        "electric" => vec!["/\\*", "/\\/*\\", "  ⚡/\\/\\⚡", "    ⚡⚡/\\⚡⚡", "      *⚡⚡*", "        ✦ZAP✦"],
        "grass" => vec!["*/*", "**//**", "  ***///***", "    **//**", "      *//*", "        ✦SLASH✦"],
        "ice" => vec!["*♦", "*♦*♦", "  ♦*♦*♦", "    ♦♦*♦♦", "      *♦*", "        ✦FREEZE✦"],
        "ghost" | "dark" | "psychic" => vec!["◎", "◎◉◎", "  ░◎◉◎░", "    ▓◎◉◎▓", "      ░◎░", "        ✦SURGE✦"],
        "dragon" | "fairy" => vec!["★✦", "★✦★✦", "  ══★✦★══", "    ═══★✦═══", "      ✦★✦", "        ✦BURST✦"],
        _ => vec!["»", "»»", "  »»»✦", "    »»»✖", "      ✦✖✦", "        ✦IMPACT✦"],
    }
}

fn animate_attack(
    player: &Fighter,
    enemy: &Fighter,
    counts: (usize, usize, usize, usize),
    mv: &BattleMove,
    attacker_name: &str,
    p_attacking: bool,
    running: &Arc<AtomicBool>,
) {
    let col = type_rgb(&mv.mtype);
    let frames = attack_fx_frames(&mv.mtype, mv.is_status);
    let msg = format!("{} used {}!", attacker_name, mv.name);
    for (i, fx) in frames.iter().enumerate() {
        if !running.load(Ordering::SeqCst) { break; }
        let shift = if p_attacking && !mv.is_status && (i == 1 || i == 2) { 3 } else { 0 };
        draw_stage(player, enemy, counts.0, counts.1, counts.2, counts.3, Some((fx, col)), &msg, &[], false, false, shift, 0, 0);
        thread::sleep(Duration::from_millis(85));
    }
}

fn animate_damage(
    player: &mut Fighter,
    enemy: &mut Fighter,
    counts: (usize, usize, usize, usize),
    target_is_enemy: bool,
    dmg: i32,
    msg: &str,
    running: &Arc<AtomicBool>,
) {
    let col = (255, 95, 95);
    let fx_txt = format!("-{} HP!", dmg);
    for blink in 0..4 {
        if !running.load(Ordering::SeqCst) { break; }
        let hide = blink % 2 == 0;
        draw_stage(
            player, enemy, counts.0, counts.1, counts.2, counts.3,
            Some((&fx_txt, col)), msg, &[],
            !target_is_enemy && hide, target_is_enemy && hide, 0, 0, 0,
        );
        thread::sleep(Duration::from_millis(75));
    }

    let start_hp = if target_is_enemy { enemy.hp } else { player.hp };
    let end_hp = (start_hp - dmg).max(0);
    let steps = 8;
    for s in 1..=steps {
        if !running.load(Ordering::SeqCst) { break; }
        let cur = start_hp - ((start_hp - end_hp) * s / steps);
        if target_is_enemy { enemy.hp = cur; } else { player.hp = cur; }
        draw_stage(player, enemy, counts.0, counts.1, counts.2, counts.3, Some((&fx_txt, col)), msg, &[], false, false, 0, 0, 0);
        thread::sleep(Duration::from_millis(45));
    }
    if target_is_enemy { enemy.hp = end_hp; } else { player.hp = end_hp; }
    thread::sleep(Duration::from_millis(320));
}

fn animate_ko(
    player: &Fighter,
    enemy: &Fighter,
    counts: (usize, usize, usize, usize),
    enemy_ko: bool,
    running: &Arc<AtomicBool>,
) {
    let fallen = if enemy_ko { enemy } else { player };
    let cry = spawn_play_cry(fallen.id, fallen.name.clone(), fallen.data["cry_url"].as_str().unwrap_or("").to_string());
    let msg = format!("{} fainted!", fallen.name);
    for step in 1..=5 {
        if !running.load(Ordering::SeqCst) { break; }
        draw_stage(
            player, enemy, counts.0, counts.1, counts.2, counts.3,
            Some(("✖ KNOCKOUT ✖", (255, 80, 80))), &msg, &[],
            false, false, 0, if enemy_ko { step } else { 0 }, if !enemy_ko { step } else { 0 },
        );
        thread::sleep(Duration::from_millis(95));
    }
    draw_stage(
        player, enemy, counts.0, counts.1, counts.2, counts.3,
        Some(("✖ KNOCKOUT ✖", (255, 80, 80))), &msg, &[],
        !enemy_ko, enemy_ko, 0, 0, 0,
    );
    if let Some(h) = cry { let _ = h.join(); }
    thread::sleep(Duration::from_millis(450));
}

fn animate_pokeball(
    player: &Fighter,
    enemy: &Fighter,
    counts: (usize, usize, usize, usize),
    caught: bool,
    running: &Arc<AtomicBool>,
) {
    let throw_frames = [
        ("●  (throwing Ultra Ball...)", (255, 220, 70)),
        ("      ●", (255, 220, 70)),
        ("            ●", (255, 220, 70)),
        ("            (o)  *wiggle left*", (255, 215, 70)),
        ("            (o)  *wiggle right*", (255, 215, 70)),
        ("            (o)  *wiggle left*", (255, 215, 70)),
    ];
    for (i, (fx, col)) in throw_frames.iter().enumerate() {
        if !running.load(Ordering::SeqCst) { break; }
        draw_stage(
            player, enemy, counts.0, counts.1, counts.2, counts.3,
            Some((fx, *col)), "You threw an Ultra Ball!", &[],
            false, i >= 3, 0, 0, 0,
        );
        thread::sleep(Duration::from_millis(if i < 3 { 140 } else { 320 }));
    }
    if caught {
        draw_stage(
            player, enemy, counts.0, counts.1, counts.2, counts.3,
            Some(("✦ ★ GOTCHA! CAPTURED! ★ ✦", (90, 235, 115))),
            &format!("{} was caught and sent to your PC Box!", enemy.name), &[],
            false, true, 0, 0, 0,
        );
    } else {
        draw_stage(
            player, enemy, counts.0, counts.1, counts.2, counts.3,
            Some(("! IT BROKE FREE !", (255, 110, 90))),
            &format!("Oh no! {} broke free!", enemy.name), &[],
            false, false, 0, 0, 0,
        );
    }
    thread::sleep(Duration::from_millis(900));
}

fn animate_victory(
    player: &Fighter,
    enemy: &Fighter,
    counts: (usize, usize, usize, usize),
    player_won: bool,
    turns: u32,
    total_dmg: i32,
    running: &Arc<AtomicBool>,
) {
    let winner = if player_won { player } else { enemy };
    let cry = spawn_play_cry(winner.id, winner.name.clone(), winner.data["cry_url"].as_str().unwrap_or("").to_string());
    let wc = type_rgb(winner.types.first().map(|s| s.as_str()).unwrap_or("gold"));
    let banner = if player_won {
        format!("VICTORY! {} won in {} turns ({} total damage)!", winner.name, turns, total_dmg)
    } else {
        format!("DEFEAT! {} defeated your team in {} turns.", winner.name, turns)
    };
    let confetti = [
        "★  ✦  *  CHAMPION  *  ✦  ★",
        "✦  *  ★  CHAMPION  ★  *  ✦",
        "*  ★  ✦  CHAMPION  ✦  ★  *",
        "★  ✦  *  VICTORY!  *  ✦  ★",
    ];
    for frame in confetti {
        if !running.load(Ordering::SeqCst) { break; }
        draw_stage(
            player, enemy, counts.0, counts.1, counts.2, counts.3,
            Some((frame, wc)), &banner, &[],
            !player_won, player_won, 0, 0, 0,
        );
        thread::sleep(Duration::from_millis(240));
    }
    if let Some(h) = cry { let _ = h.join(); }
    thread::sleep(Duration::from_millis(800));
}

fn read_input(prompt: &str) -> Option<String> {
    print!("  {}", prompt);
    let _ = io::stdout().flush();
    let mut s = String::new();
    if io::stdin().read_line(&mut s).is_err() {
        return None;
    }
    Some(s.trim().to_string())
}

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

fn resolve_turn_action(
    player: &mut Fighter,
    enemy: &mut Fighter,
    p_attacking: bool,
    move_idx: usize,
    counts: (usize, usize, usize, usize),
    total_dmg: &mut i32,
    running: &Arc<AtomicBool>,
) {
    let mv = if p_attacking {
        if let Some(m) = player.moves.get_mut(move_idx) {
            if m.pp > 0 { m.pp -= 1; }
            m.clone()
        } else { return; }
    } else {
        if let Some(m) = enemy.moves.get_mut(move_idx) {
            if m.pp > 0 { m.pp -= 1; }
            m.clone()
        } else { return; }
    };

    let r = rand_seed();
    {
        let atk = if p_attacking { &mut *player } else { &mut *enemy };
        match atk.status {
            Status::Sleep(rem) => {
                if rem <= 1 {
                    atk.status = Status::None;
                } else {
                    atk.status = Status::Sleep(rem - 1);
                    let msg = format!("{} is fast asleep!", atk.name);
                    draw_stage(player, enemy, counts.0, counts.1, counts.2, counts.3, Some(("zZz...", (140, 180, 255))), &msg, &[], false, false, 0, 0, 0);
                    thread::sleep(Duration::from_millis(650));
                    return;
                }
            }
            Status::Freeze => {
                if (r % 5) == 0 {
                    atk.status = Status::None;
                } else {
                    let msg = format!("{} is frozen solid!", atk.name);
                    draw_stage(player, enemy, counts.0, counts.1, counts.2, counts.3, Some(("❄ FROZEN ❄", (120, 230, 245))), &msg, &[], false, false, 0, 0, 0);
                    thread::sleep(Duration::from_millis(650));
                    return;
                }
            }
            Status::Paralyze if (r % 4) == 0 => {
                let msg = format!("{} is paralyzed and can't move!", atk.name);
                draw_stage(player, enemy, counts.0, counts.1, counts.2, counts.3, Some(("⚡ PARALYZED ⚡", (255, 215, 45))), &msg, &[], false, false, 0, 0, 0);
                thread::sleep(Duration::from_millis(650));
                return;
            }
            _ => {}
        }
    }

    let atk_name = if p_attacking { player.name.clone() } else { enemy.name.clone() };
    animate_attack(player, enemy, counts, &mv, &atk_name, p_attacking, running);

    if !mv.target_self && ((r % 100) as i32) >= mv.accuracy {
        let msg = format!("{} used {} — but it missed!", atk_name, mv.name);
        draw_stage(player, enemy, counts.0, counts.1, counts.2, counts.3, Some(("MISS!", (180, 185, 195))), &msg, &[], false, false, 0, 0, 0);
        thread::sleep(Duration::from_millis(650));
        return;
    }

    if mv.is_status {
        let mut msg = format!("{} used {}!", atk_name, mv.name);
        if mv.healing > 0 {
            let heal = {
                let atk = if p_attacking { &mut *player } else { &mut *enemy };
                let h = (atk.max_hp * mv.healing / 100).max(20);
                atk.hp = (atk.hp + h).min(atk.max_hp);
                h
            };
            msg = format!("{} used {} and recovered +{} HP!", atk_name, mv.name, heal);
        } else if !mv.stat_changes.is_empty() {
            let note = if mv.target_self {
                let atk = if p_attacking { &mut *player } else { &mut *enemy };
                apply_stat_changes(atk, &mv.stat_changes)
            } else {
                let def = if p_attacking { &mut *enemy } else { &mut *player };
                apply_stat_changes(def, &mv.stat_changes)
            };
            if !note.is_empty() {
                msg = format!("{} used {} ({})!", atk_name, mv.name, note);
            }
        } else {
            let def = if p_attacking { &mut *enemy } else { &mut *player };
            if def.status == Status::None {
                match mv.ailment.as_str() {
                    "burn" => { def.status = Status::Burn; msg = format!("{} was burned!", def.name); }
                    "paralysis" => { def.status = Status::Paralyze; msg = format!("{} was paralyzed!", def.name); }
                    "sleep" => { def.status = Status::Sleep(((r % 3) + 1) as u8); msg = format!("{} fell asleep!", def.name); }
                    "poison" => { def.status = Status::Poison(1); msg = format!("{} was poisoned!", def.name); }
                    "freeze" => { def.status = Status::Freeze; msg = format!("{} was frozen!", def.name); }
                    _ => {}
                }
            } else {
                msg = format!("{} already has a status condition!", def.name);
            }
        }
        draw_stage(player, enemy, counts.0, counts.1, counts.2, counts.3, Some(("✦ EFFECT ✦", type_rgb(&mv.mtype))), &msg, &[], false, false, 0, 0, 0);
        thread::sleep(Duration::from_millis(700));
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
        let msg = format!("{}'s {} negated {}!", def.name, ab, mv.name);
        draw_stage(player, enemy, counts.0, counts.1, counts.2, counts.3, Some(("BLOCKED!", (110, 235, 255))), &msg, &[], false, false, 0, 0, 0);
        thread::sleep(Duration::from_millis(700));
        return;
    }

    if eff == 0.0 {
        let msg = format!("It doesn't affect {}...", if p_attacking { &enemy.name } else { &player.name });
        draw_stage(player, enemy, counts.0, counts.1, counts.2, counts.3, Some(("NO EFFECT", (160, 165, 175))), &msg, &[], false, false, 0, 0, 0);
        thread::sleep(Duration::from_millis(650));
        return;
    }

    let mut clutch_note = "";
    {
        let def = if p_attacking { &mut *enemy } else { &mut *player };
        if dmg >= def.hp {
            if def.hp == def.max_hp && ability_is(def, "sturdy") {
                dmg = def.hp - 1;
                clutch_note = " Held on with Sturdy!";
            } else if !p_attacking && !def.bond_used && (r % 10 == 0) && def.hp > 1 {
                dmg = def.hp - 1;
                def.bond_used = true;
                clutch_note = " Toughed it out at 1 HP!";
            }
        }
    }

    if p_attacking { *total_dmg += dmg; }

    let eff_txt = if eff >= 3.9 { " 4x MEGA EFFECTIVE!" }
        else if eff >= 1.9 { " Super effective!" }
        else if eff <= 0.51 { " Not very effective..." }
        else { "" };
    let crit_txt = if crit { " CRITICAL HIT!" } else { "" };
    let hit_msg = format!("{}'s {} dealt {} HP!{}{}{}", atk_name, mv.name, dmg, crit_txt, eff_txt, clutch_note);

    animate_damage(player, enemy, counts, p_attacking, dmg, &hit_msg, running);

    if mv.drain > 0 && dmg > 0 {
        let heal = (dmg * mv.drain / 100).max(1);
        let atk = if p_attacking { &mut *player } else { &mut *enemy };
        atk.hp = (atk.hp + heal).min(atk.max_hp);
    } else if mv.drain < 0 && dmg > 0 {
        let recoil = (dmg * (-mv.drain) / 100).max(1);
        let atk = if p_attacking { &mut *player } else { &mut *enemy };
        atk.hp = (atk.hp - recoil).max(0);
    }

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

pub fn run_fight_mode(p1_spec: Option<&str>, p2_spec: Option<&str>) {
    let p1_names = match p1_spec {
        Some(s) => parse_team_arg(s),
        None => vec![
            ((rand_seed() % 493) + 1).to_string(),
            (((rand_seed().wrapping_mul(31)) % 493) + 1).to_string(),
            (((rand_seed().wrapping_mul(97)) % 493) + 1).to_string(),
        ],
    };
    let p2_names = match p2_spec {
        Some(s) => parse_team_arg(s),
        None => (0..p1_names.len().max(1))
            .map(|i| (((rand_seed().wrapping_add(i as u32 * 137)) % 898) + 1).to_string())
            .collect(),
    };

    let pb = ProgressBar::new_spinner();
    pb.set_style(ProgressStyle::default_spinner().template("{spinner:.yellow} Preparing arena, movesets & animations...").unwrap());
    pb.enable_steady_tick(Duration::from_millis(80));

    let load_team = |names: Vec<String>| -> Vec<Fighter> {
        thread::scope(|s| {
            let hs: Vec<_> = names
                .into_iter()
                .map(|n| s.spawn(move || fetch_pokemon(&n, false).map(build_fighter)))
                .collect();
            hs.into_iter().filter_map(|h| h.join().ok().flatten()).collect()
        })
    };

    let (mut p_team, mut e_team) = thread::scope(|s| {
        let h1 = s.spawn(|| load_team(p1_names));
        let h2 = s.spawn(|| load_team(p2_names));
        (h1.join().unwrap_or_default(), h2.join().unwrap_or_default())
    });
    pb.finish_and_clear();

    if p_team.is_empty() || e_team.is_empty() {
        eprintln!("  {}", "Could not load teams for --fight. Example: pokefact --fight gengar lucario".red().bold());
        return;
    }

    let running = Arc::new(AtomicBool::new(true));
    let r_clone = Arc::clone(&running);
    let _ = ctrlc::set_handler(move || {
        r_clone.store(false, Ordering::SeqCst);
    });

    let _guard = ScreenGuard;
    let mut out = io::stdout();
    let _ = write!(out, "\x1b[?1049h\x1b[?25l\x1b[2J");
    let _ = out.flush();

    let (mut p_idx, mut e_idx) = (0usize, 0usize);
    let mut bag = Bag { potions: 2, heals: 2, tonics: 1, balls: 3 };
    let mut turn = 1u32;
    let mut total_dmg = 0i32;

    if ability_is(&p_team[p_idx], "intimidate") { e_team[e_idx].stages[0] = -1; }
    if ability_is(&e_team[e_idx], "intimidate") { p_team[p_idx].stages[0] = -1; }

    let mut status_msg = format!(
        "Battle start! {} vs {} (Canon Fair-Play active).",
        p_team[p_idx].name, e_team[e_idx].name
    );

    while running.load(Ordering::SeqCst) {
        let p_alive = p_team.iter().filter(|f| f.hp > 0).count();
        let e_alive = e_team.iter().filter(|f| f.hp > 0).count();
        let counts = (p_alive, p_team.len(), e_alive, e_team.len());

        let (tw, _) = term_dims();
        let inner = tw.saturating_sub(4).clamp(30, 94);
        let main_menu = vec![
            pad_right(&format!("[1] FIGHT ({} moves)    [2] BAG ({} items)", p_team[p_idx].moves.len(), bag.potions + bag.heals + bag.tonics + bag.balls), inner),
            pad_right(&format!("[3] PARTY ({}/{} alive)  [4] RUN", p_alive, p_team.len()), inner),
        ];

        draw_stage(&p_team[p_idx], &e_team[e_idx], counts.0, counts.1, counts.2, counts.3, None, &status_msg, &main_menu, false, false, 0, 0, 0);
        let _ = write!(io::stdout(), "\x1b[?25h");
        let _ = io::stdout().flush();

        let Some(act) = read_input(&format!("Turn {} — Choose (1-4): ", turn)) else { break; };
        let _ = write!(io::stdout(), "\x1b[?25l");
        let _ = io::stdout().flush();

        enum PlayerAction {
            Move(usize),
            Switch(usize),
            Potion,
            Heal,
            Tonic,
            Ball,
        }

        let chosen: Option<PlayerAction> = match act.as_str() {
            "4" | "q" | "Q" => {
                draw_stage(&p_team[p_idx], &e_team[e_idx], counts.0, counts.1, counts.2, counts.3, None, "Got away safely!", &[], false, false, 0, 0, 0);
                thread::sleep(Duration::from_millis(600));
                break;
            }
            "1" => {
                let mut page = 0usize;
                let per_page = 4usize;
                let mut picked = None;
                loop {
                    let moves = &p_team[p_idx].moves;
                    let total_pages = (moves.len() + per_page - 1) / per_page;
                    let start = page * per_page;
                    let end = (start + per_page).min(moves.len());
                    let mut m_lines = Vec::new();
                    let mults = defensive_multipliers(&e_team[e_idx].types);
                    for (slot, mv) in moves[start..end].iter().enumerate() {
                        let cat = if mv.is_status { "Stat" } else if mv.is_physical { "Phys" } else { "Spec" };
                        let eff_badge = if mv.is_status {
                            ""
                        } else {
                            let m = mults.iter().find(|(k, _)| k.eq_ignore_ascii_case(&mv.mtype)).map(|(_, &v)| v).unwrap_or(1.0);
                            if m >= 3.9 { " [4x]" } else if m >= 1.9 { " [2x]" } else if m == 0.0 { " [0x]" } else if m <= 0.51 { " [0.5x]" } else { "" }
                        };
                        let stab_badge = if !mv.is_status && p_team[p_idx].types.iter().any(|t| t.eq_ignore_ascii_case(&mv.mtype)) { " [STAB]" } else { "" };
                        let header = format!(
                            "[{}] {} ({} | {} | {} BP | {}/{} PP) [{}]{}{}",
                            slot + 1, mv.name, mv.mtype, cat, mv.power, mv.pp, mv.max_pp, mv.source, eff_badge, stab_badge
                        );
                        let mc = type_rgb(&mv.mtype);
                        m_lines.push(pad_right(&header, inner).truecolor(mc.0, mc.1, mc.2).bold().to_string());
                        m_lines.push(pad_right(&format!("    {}", mv.desc), inner).dimmed().to_string());
                    }
                    m_lines.push(pad_right(&format!("Page {}/{} — [1-{}] Use | [N] Next | [P] Prev | [B] Back", page + 1, total_pages, end - start), inner).yellow().to_string());

                    draw_stage(&p_team[p_idx], &e_team[e_idx], counts.0, counts.1, counts.2, counts.3, None, "Select an attack:", &m_lines, false, false, 0, 0, 0);
                    let _ = write!(io::stdout(), "\x1b[?25h");
                    let _ = io::stdout().flush();
                    let Some(m_in) = read_input("Move > ") else { break; };
                    let _ = write!(io::stdout(), "\x1b[?25l");
                    let _ = io::stdout().flush();
                    if m_in.eq_ignore_ascii_case("b") { break; }
                    if m_in.eq_ignore_ascii_case("n") { page = (page + 1) % total_pages; continue; }
                    if m_in.eq_ignore_ascii_case("p") { page = (page + total_pages - 1) % total_pages; continue; }
                    if let Ok(n) = m_in.parse::<usize>() {
                        if n >= 1 && n <= end - start {
                            let global_idx = start + (n - 1);
                            if moves[global_idx].pp > 0 {
                                picked = Some(PlayerAction::Move(global_idx));
                                break;
                            }
                        }
                    }
                }
                picked
            }
            "2" => {
                let b_lines = vec![
                    pad_right(&format!("[1] Hyper Potion (x{}) — Restores 120 HP", bag.potions), inner),
                    pad_right(&format!("[2] Full Heal    (x{}) — Cures burn/para/sleep/poison/freeze", bag.heals), inner),
                    pad_right(&format!("[3] Power Tonic  (x{}) — Raises Attack & Sp. Atk by +1", bag.tonics), inner),
                    pad_right(&format!("[4] Ultra Ball   (x{}) — Try catching opponent into PC Box", bag.balls), inner),
                    pad_right("[B] Back", inner),
                ];
                draw_stage(&p_team[p_idx], &e_team[e_idx], counts.0, counts.1, counts.2, counts.3, None, "Choose an item from your Bag:", &b_lines, false, false, 0, 0, 0);
                let _ = write!(io::stdout(), "\x1b[?25h");
                let _ = io::stdout().flush();
                let b_in = read_input("Item > ").unwrap_or_default();
                let _ = write!(io::stdout(), "\x1b[?25l");
                let _ = io::stdout().flush();
                match b_in.as_str() {
                    "1" if bag.potions > 0 => Some(PlayerAction::Potion),
                    "2" if bag.heals > 0 => Some(PlayerAction::Heal),
                    "3" if bag.tonics > 0 => Some(PlayerAction::Tonic),
                    "4" if bag.balls > 0 => Some(PlayerAction::Ball),
                    _ => None,
                }
            }
            "3" => {
                let mut p_lines = Vec::new();
                for (i, f) in p_team.iter().enumerate() {
                    let tag = if i == p_idx { " [ACTIVE]" } else if f.hp == 0 { " [FAINTED]" } else { "" };
                    p_lines.push(pad_right(&format!("[{}] {} — {}/{} HP{}{}", i + 1, f.name, f.hp, f.max_hp, f.status.tag(), tag), inner));
                }
                p_lines.push(pad_right("[B] Back", inner));
                draw_stage(&p_team[p_idx], &e_team[e_idx], counts.0, counts.1, counts.2, counts.3, None, "Switch to which Pokemon?", &p_lines, false, false, 0, 0, 0);
                let _ = write!(io::stdout(), "\x1b[?25h");
                let _ = io::stdout().flush();
                let s_in = read_input("Switch > ").unwrap_or_default();
                let _ = write!(io::stdout(), "\x1b[?25l");
                let _ = io::stdout().flush();
                s_in.parse::<usize>()
                    .ok()
                    .and_then(|n| n.checked_sub(1))
                    .filter(|&i| i < p_team.len() && i != p_idx && p_team[i].hp > 0)
                    .map(PlayerAction::Switch)
            }
            _ => None,
        };

        let Some(action) = chosen else { continue; };

        let cpu_move = e_team[e_idx]
            .moves
            .iter()
            .enumerate()
            .filter(|(_, m)| m.pp > 0)
            .max_by_key(|(_, m)| {
                if m.is_status && turn == 1 && p_team[p_idx].status == Status::None {
                    90
                } else {
                    calc_move_damage(m, &e_team[e_idx], &p_team[p_idx], false, 0.92).0
                }
            })
            .map(|(i, _)| i)
            .unwrap_or(0);

        match action {
            PlayerAction::Switch(new_i) => {
                p_team[p_idx].stages = [0; 5];
                p_idx = new_i;
                if ability_is(&p_team[p_idx], "intimidate") { e_team[e_idx].stages[0] = (e_team[e_idx].stages[0] - 1).clamp(-6, 6); }
                status_msg = format!("Go, {}!", p_team[p_idx].name);
                draw_stage(&p_team[p_idx], &e_team[e_idx], counts.0, counts.1, counts.2, counts.3, None, &status_msg, &[], false, false, 0, 0, 0);
                thread::sleep(Duration::from_millis(500));
                resolve_turn_action(&mut p_team[p_idx], &mut e_team[e_idx], false, cpu_move, counts, &mut total_dmg, &running);
            }
            PlayerAction::Potion => {
                bag.potions -= 1;
                p_team[p_idx].hp = (p_team[p_idx].hp + 120).min(p_team[p_idx].max_hp);
                draw_stage(&p_team[p_idx], &e_team[e_idx], counts.0, counts.1, counts.2, counts.3, Some(("+120 HP", (90, 235, 115))), &format!("Used Hyper Potion on {}!", p_team[p_idx].name), &[], false, false, 0, 0, 0);
                thread::sleep(Duration::from_millis(650));
                resolve_turn_action(&mut p_team[p_idx], &mut e_team[e_idx], false, cpu_move, counts, &mut total_dmg, &running);
            }
            PlayerAction::Heal => {
                bag.heals -= 1;
                p_team[p_idx].status = Status::None;
                draw_stage(&p_team[p_idx], &e_team[e_idx], counts.0, counts.1, counts.2, counts.3, Some(("CURED!", (110, 235, 255))), &format!("{}'s status was cured!", p_team[p_idx].name), &[], false, false, 0, 0, 0);
                thread::sleep(Duration::from_millis(650));
                resolve_turn_action(&mut p_team[p_idx], &mut e_team[e_idx], false, cpu_move, counts, &mut total_dmg, &running);
            }
            PlayerAction::Tonic => {
                bag.tonics -= 1;
                p_team[p_idx].stages[0] = (p_team[p_idx].stages[0] + 1).clamp(-6, 6);
                p_team[p_idx].stages[2] = (p_team[p_idx].stages[2] + 1).clamp(-6, 6);
                draw_stage(&p_team[p_idx], &e_team[e_idx], counts.0, counts.1, counts.2, counts.3, Some(("POWER UP!", (255, 215, 70))), &format!("{}'s Attack & Sp. Atk rose!", p_team[p_idx].name), &[], false, false, 0, 0, 0);
                thread::sleep(Duration::from_millis(650));
                resolve_turn_action(&mut p_team[p_idx], &mut e_team[e_idx], false, cpu_move, counts, &mut total_dmg, &running);
            }
            PlayerAction::Ball => {
                bag.balls -= 1;
                let hp_factor = 1.0 - (e_team[e_idx].hp as f32 / e_team[e_idx].max_hp as f32);
                let st_bonus = if e_team[e_idx].status != Status::None { 0.22 } else { 0.0 };
                let chance = ((0.18 + hp_factor * 0.65 + st_bonus) * 100.0).clamp(15.0, 95.0) as u32;
                let caught = (rand_seed() % 100) < chance;
                animate_pokeball(&p_team[p_idx], &e_team[e_idx], counts, caught, &running);
                if caught {
                    catch_pokemon(&e_team[e_idx].data);
                    e_team[e_idx].hp = 0;
                } else {
                    resolve_turn_action(&mut p_team[p_idx], &mut e_team[e_idx], false, cpu_move, counts, &mut total_dmg, &running);
                }
            }
            PlayerAction::Move(p_mv) => {
                let p_prio = p_team[p_idx].moves[p_mv].priority;
                let e_prio = e_team[e_idx].moves[cpu_move].priority;
                let p_first = if p_prio != e_prio {
                    p_prio > e_prio
                } else {
                    effective_speed(&p_team[p_idx]) >= effective_speed(&e_team[e_idx])
                };

                if p_first {
                    resolve_turn_action(&mut p_team[p_idx], &mut e_team[e_idx], true, p_mv, counts, &mut total_dmg, &running);
                    if e_team[e_idx].hp > 0 && p_team[p_idx].hp > 0 {
                        resolve_turn_action(&mut p_team[p_idx], &mut e_team[e_idx], false, cpu_move, counts, &mut total_dmg, &running);
                    }
                } else {
                    resolve_turn_action(&mut p_team[p_idx], &mut e_team[e_idx], false, cpu_move, counts, &mut total_dmg, &running);
                    if p_team[p_idx].hp > 0 && e_team[e_idx].hp > 0 {
                        resolve_turn_action(&mut p_team[p_idx], &mut e_team[e_idx], true, p_mv, counts, &mut total_dmg, &running);
                    }
                }
            }
        }

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

        if e_team[e_idx].hp == 0 {
            animate_ko(&p_team[p_idx], &e_team[e_idx], counts, true, &running);
            if let Some(next_e) = e_team.iter().position(|f| f.hp > 0) {
                e_idx = next_e;
                if ability_is(&e_team[e_idx], "intimidate") { p_team[p_idx].stages[0] = (p_team[p_idx].stages[0] - 1).clamp(-6, 6); }
                status_msg = format!("CPU sent out {}!", e_team[e_idx].name);
            } else {
                animate_victory(&p_team[p_idx], &e_team[e_idx], (p_alive, p_team.len(), 0, e_team.len()), true, turn, total_dmg, &running);
                break;
            }
        }

        if p_team[p_idx].hp == 0 {
            animate_ko(&p_team[p_idx], &e_team[e_idx], counts, false, &running);
            if let Some(next_p) = p_team.iter().position(|f| f.hp > 0) {
                p_idx = next_p;
                if ability_is(&p_team[p_idx], "intimidate") { e_team[e_idx].stages[0] = (e_team[e_idx].stages[0] - 1).clamp(-6, 6); }
                status_msg = format!("You sent out {}!", p_team[p_idx].name);
            } else {
                animate_victory(&p_team[p_idx], &e_team[e_idx], (0, p_team.len(), e_alive, e_team.len()), false, turn, total_dmg, &running);
                break;
            }
        } else {
            status_msg = format!("What will {} do?", p_team[p_idx].name);
        }

        turn += 1;
    }
}
