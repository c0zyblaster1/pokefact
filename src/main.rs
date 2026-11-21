mod graphics;
mod modes;

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io::{Cursor, Read};
use std::path::PathBuf;
use std::thread;
use colored::*;
use indicatif::{ProgressBar, ProgressStyle};
use regex::Regex;
use rodio::{Decoder, OutputStream, Sink};
use serde_json::Value;

pub type Rgb = (u8, u8, u8);

#[derive(Clone, Copy)]
pub struct Theme {
    pub primary: Rgb,
    pub secondary: Rgb,
}

pub fn type_rgb(t: &str) -> Rgb {
    match t.to_lowercase().as_str() {
        "normal"   => (180, 180, 155),
        "fire"     => (255, 105, 50),
        "water"    => (70, 155, 255),
        "electric" => (255, 215, 45),
        "grass"    => (95, 210, 95),
        "ice"      => (120, 230, 245),
        "fighting" => (220, 70, 60),
        "poison"   => (185, 80, 225),
        "ground"   => (225, 185, 105),
        "flying"   => (155, 170, 255),
        "psychic"  => (255, 95, 155),
        "bug"      => (165, 195, 45),
        "rock"     => (195, 170, 90),
        "ghost"    => (145, 105, 235),
        "dragon"   => (125, 90, 255),
        "dark"     => (135, 110, 95),
        "steel"    => (180, 195, 215),
        "fairy"    => (255, 150, 205),
        _          => (110, 220, 255),
    }
}

pub fn theme_for_types(types: &[String]) -> Theme {
    let p = types.first().map(|t| type_rgb(t)).unwrap_or((110, 220, 255));
    let s = types.get(1).map(|t| type_rgb(t)).unwrap_or((
        p.0.saturating_add(45),
        p.1.saturating_add(45),
        p.2.saturating_add(45),
    ));
    Theme { primary: p, secondary: s }
}

pub fn term_width() -> usize {
    terminal_size::terminal_size()
        .map(|(terminal_size::Width(w), _)| w as usize)
        .unwrap_or(112)
        .max(36)
}

pub fn pad_right(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width { s.chars().take(width).collect() } else { format!("{}{}", s, " ".repeat(width - len)) }
}

pub fn pad_left(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width { s.chars().take(width).collect() } else { format!("{}{}", " ".repeat(width - len), s) }
}

pub fn center_str(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width { return s.chars().take(width).collect(); }
    let total = width - len;
    let left = total / 2;
    format!("{}{}{}", " ".repeat(left), s, " ".repeat(total - left))
}

pub fn wrap_text(text: &str, max_width: usize) -> Vec<String> {
    let max_width = max_width.max(12);
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if current.is_empty() {
            current.push_str(word);
        } else if current.chars().count() + 1 + word.chars().count() <= max_width {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(current);
            current = word.to_string();
        }
    }
    if !current.is_empty() { lines.push(current); }
    if lines.is_empty() { lines.push(String::new()); }
    lines
}

fn cache_path() -> PathBuf {
    let mut dir = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    dir.push(".pokefact_cache.json");
    dir
}

fn names_cache_path() -> PathBuf {
    let mut dir = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    dir.push(".pokefact_names.json");
    dir
}

fn cries_dir() -> PathBuf {
    let mut dir = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    dir.push(".pokefact_cries");
    let _ = fs::create_dir_all(&dir);
    dir
}

fn load_cache(query: &str) -> Option<Value> {
    let content = fs::read_to_string(cache_path()).ok()?;
    serde_json::from_str::<Value>(&content).ok()?.get(query).cloned()
}

fn save_cache(query: &str, data: &Value) {
    let path = cache_path();
    let mut cache = fs::read_to_string(&path)
        .ok()
        .and_then(|c| serde_json::from_str::<Value>(&c).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    if let Some(obj) = cache.as_object_mut() {
        obj.insert(query.to_string(), data.clone());
    }
    let _ = fs::write(path, serde_json::to_string(&cache).unwrap_or_default());
}

pub fn fetch_bytes(url: &str) -> Option<Vec<u8>> {
    if url.is_empty() { return None; }
    let resp = ureq::get(url).set("User-Agent", "pokefact/1.0").call().ok()?;
    let mut bytes = Vec::new();
    resp.into_reader().read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

pub fn spawn_play_cry(id: i64, name: String, cry_url: String) -> Option<thread::JoinHandle<()>> {
    Some(thread::spawn(move || {
        let slug: String = name.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect();
        let mp3_file = cries_dir().join(format!("{}.mp3", id));
        let ogg_file = cries_dir().join(format!("{}.ogg", id));
        let showdown_url = format!("https://play.pokemonshowdown.com/audio/cries/{}.mp3", slug);

        let play_bytes = |bytes: Vec<u8>| -> bool {
            let Ok((_stream, handle)) = OutputStream::try_default() else { return false; };
            let Ok(sink) = Sink::try_new(&handle) else { return false; };
            let Ok(source) = Decoder::new(Cursor::new(bytes)) else { return false; };
            sink.set_volume(1.0);
            sink.append(source);
            sink.sleep_until_end();
            true
        };

        if let Ok(b) = fs::read(&mp3_file) { if play_bytes(b) { return; } }
        else if let Some(b) = fetch_bytes(&showdown_url) { let _ = fs::write(&mp3_file, &b); if play_bytes(b) { return; } }

        if !cry_url.is_empty() {
            if let Ok(b) = fs::read(&ogg_file) { if play_bytes(b) { return; } }
            else if let Some(b) = fetch_bytes(&cry_url) { let _ = fs::write(&ogg_file, &b); if play_bytes(b) { return; } }
        }
    }))
}

pub fn levenshtein(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let (m, n) = (a_chars.len(), b_chars.len());
    let mut dp = vec![vec![0usize; n + 1]; m + 1];
    for i in 0..=m { dp[i][0] = i; }
    for j in 0..=n { dp[0][j] = j; }
    for i in 1..=m {
        for j in 1..=n {
            let cost = if a_chars[i - 1] == b_chars[j - 1] { 0 } else { 1 };
            dp[i][j] = (dp[i - 1][j] + 1).min(dp[i][j - 1] + 1).min(dp[i - 1][j - 1] + cost);
        }
    }
    dp[m][n]
}

pub fn fuzzy_match(guess: &str, target: &str) -> (usize, f32) {
    let norm = |s: &str| -> String { s.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect() };
    let (g, t) = (norm(guess), norm(target));
    if g.is_empty() || t.is_empty() { return (t.len(), 0.0); }
    let dist = levenshtein(&g, &t);
    let sim = (1.0 - (dist as f32 / g.len().max(t.len()) as f32)).max(0.0);
    (dist, sim)
}

fn autocorrect_name(query: &str) -> Option<String> {
    if query.chars().all(|c| c.is_ascii_digit()) { return None; }
    let path = names_cache_path();
    let names: Vec<String> = if let Ok(c) = fs::read_to_string(&path) {
        serde_json::from_str(&c).unwrap_or_default()
    } else {
        let json: Value = ureq::get("https://pokeapi.co/api/v2/pokemon-species?limit=1025").call().ok()?.into_json().ok()?;
        let list: Vec<String> = json["results"].as_array()?.iter().filter_map(|r| r["name"].as_str().map(String::from)).collect();
        let _ = fs::write(path, serde_json::to_string(&list).unwrap_or_default());
        list
    };

    let mut best: Option<(String, f32)> = None;
    for candidate in names {
        let (dist, sim) = fuzzy_match(query, &candidate);
        if sim >= 0.65 && dist <= 3 {
            if best.as_ref().map(|(_, s)| sim > *s).unwrap_or(true) {
                best = Some((candidate, sim));
            }
        }
    }
    best.map(|(n, _)| n)
}

pub fn defensive_multipliers(types: &[String]) -> HashMap<&'static str, f32> {
    let all_types = [
        "Normal", "Fire", "Water", "Electric", "Grass", "Ice", "Fighting", "Poison", "Ground",
        "Flying", "Psychic", "Bug", "Rock", "Ghost", "Dragon", "Dark", "Steel", "Fairy",
    ];
    let mut mults: HashMap<&str, f32> = all_types.iter().map(|&t| (t, 1.0)).collect();
    for def_t in types {
        let (weak, resist, immune): (&[&str], &[&str], &[&str]) = match def_t.to_lowercase().as_str() {
            "normal"   => (&["Fighting"], &[], &["Ghost"]),
            "fire"     => (&["Water", "Ground", "Rock"], &["Fire", "Grass", "Ice", "Bug", "Steel", "Fairy"], &[]),
            "water"    => (&["Electric", "Grass"], &["Fire", "Water", "Ice", "Steel"], &[]),
            "electric" => (&["Ground"], &["Electric", "Flying", "Steel"], &[]),
            "grass"    => (&["Fire", "Ice", "Poison", "Flying", "Bug"], &["Water", "Electric", "Grass", "Ground"], &[]),
            "ice"      => (&["Fire", "Fighting", "Rock", "Steel"], &["Ice"], &[]),
            "fighting" => (&["Flying", "Psychic", "Fairy"], &["Bug", "Rock", "Dark"], &[]),
            "poison"   => (&["Ground", "Psychic"], &["Grass", "Fighting", "Poison", "Bug", "Fairy"], &[]),
            "ground"   => (&["Water", "Grass", "Ice"], &["Poison", "Rock"], &["Electric"]),
            "flying"   => (&["Electric", "Ice", "Rock"], &["Grass", "Fighting", "Bug"], &["Ground"]),
            "psychic"  => (&["Bug", "Ghost", "Dark"], &["Fighting", "Psychic"], &[]),
            "bug"      => (&["Fire", "Flying", "Rock"], &["Grass", "Fighting", "Ground"], &[]),
            "rock"     => (&["Water", "Grass", "Fighting", "Ground", "Steel"], &["Normal", "Fire", "Poison", "Flying"], &[]),
            "ghost"    => (&["Ghost", "Dark"], &["Poison", "Bug"], &["Normal", "Fighting"]),
            "dragon"   => (&["Ice", "Dragon", "Fairy"], &["Fire", "Water", "Electric", "Grass"], &[]),
            "dark"     => (&["Fighting", "Bug", "Fairy"], &["Ghost", "Dark"], &["Psychic"]),
            "steel"    => (&["Fire", "Fighting", "Ground"], &["Normal", "Grass", "Ice", "Flying", "Psychic", "Bug", "Rock", "Dragon", "Steel", "Fairy"], &["Poison"]),
            "fairy"    => (&["Poison", "Steel"], &["Fighting", "Bug", "Dark"], &["Dragon"]),
            _          => (&[], &[], &[]),
        };
        for &w in weak { *mults.entry(w).or_insert(1.0) *= 2.0; }
        for &r in resist { *mults.entry(r).or_insert(1.0) *= 0.5; }
        for &i in immune { *mults.entry(i).or_insert(1.0) *= 0.0; }
    }
    mults
}

pub fn type_matchups(types: &[String]) -> (Vec<String>, Vec<String>, Vec<String>, Vec<String>, Vec<String>) {
    let all_types = [
        "Normal", "Fire", "Water", "Electric", "Grass", "Ice", "Fighting", "Poison", "Ground",
        "Flying", "Psychic", "Bug", "Rock", "Ghost", "Dragon", "Dark", "Steel", "Fairy",
    ];
    let mults = defensive_multipliers(types);
    let (mut x4, mut x2, mut h, mut q, mut imm) = (vec![], vec![], vec![], vec![], vec![]);
    for &t in &all_types {
        let m = mults[t];
        if m >= 3.9 { x4.push(t.to_string()); }
        else if m >= 1.9 { x2.push(t.to_string()); }
        else if m == 0.0 { imm.push(t.to_string()); }
        else if m <= 0.26 { q.push(t.to_string()); }
        else if m <= 0.51 { h.push(t.to_string()); }
    }
    (x4, x2, h, q, imm)
}

pub fn best_stab_multiplier(attacker_types: &[String], defender_types: &[String]) -> (String, f32) {
    let def_mults = defensive_multipliers(defender_types);
    let mut best_t = attacker_types.first().cloned().unwrap_or_else(|| "Normal".to_string());
    let mut best_m = -1.0f32;
    for atk_t in attacker_types {
        if let Some((&k, &m)) = def_mults.iter().find(|(k, _)| k.eq_ignore_ascii_case(atk_t)) {
            if m > best_m { best_m = m; best_t = k.to_string(); }
        }
    }
    (best_t, best_m.max(0.0))
}

pub fn signature_move_for_type(t: &str, physical: bool) -> &'static str {
    match (t.to_lowercase().as_str(), physical) {
        ("fire", true) => "Flare Blitz", ("fire", false) => "Flamethrower",
        ("water", true) => "Liquidation", ("water", false) => "Hydro Pump",
        ("electric", true) => "Wild Charge", ("electric", false) => "Thunderbolt",
        ("grass", true) => "Leaf Blade", ("grass", false) => "Energy Ball",
        ("ice", true) => "Icicle Crash", ("ice", false) => "Ice Beam",
        ("fighting", true) => "Close Combat", ("fighting", false) => "Aura Sphere",
        ("poison", true) => "Gunk Shot", ("poison", false) => "Sludge Bomb",
        ("ground", true) => "Earthquake", ("ground", false) => "Earth Power",
        ("flying", true) => "Brave Bird", ("flying", false) => "Air Slash",
        ("psychic", true) => "Psycho Cut", ("psychic", false) => "Psychic",
        ("bug", true) => "X-Scissor", ("bug", false) => "Bug Buzz",
        ("rock", true) => "Stone Edge", ("rock", false) => "Power Gem",
        ("ghost", true) => "Poltergeist", ("ghost", false) => "Shadow Ball",
        ("dragon", true) => "Outrage", ("dragon", false) => "Draco Meteor",
        ("dark", true) => "Crunch", ("dark", false) => "Dark Pulse",
        ("steel", true) => "Meteor Mash", ("steel", false) => "Flash Cannon",
        ("fairy", true) => "Play Rough", ("fairy", false) => "Moonblast",
        (_, true) => "Double-Edge", (_, false) => "Hyper Voice",
    }
}

pub fn get_stat(stats: &[(String, i32)], key: &str) -> i32 {
    stats.iter().find(|(n, _)| n.eq_ignore_ascii_case(key)).map(|(_, v)| *v).unwrap_or(0)
}

pub fn detect_archetype(stats: &[(String, i32)]) -> &'static str {
    let (hp, atk, def, spa, spd, spe) = (
        get_stat(stats, "HP"), get_stat(stats, "Attack"), get_stat(stats, "Defense"),
        get_stat(stats, "Sp. Atk"), get_stat(stats, "Sp. Def"), get_stat(stats, "Speed"),
    );
    if hp == 1 { return "Wonder Guard Anomaly"; }
    if spe >= 100 && spa >= 105 && spa > atk { return "Special Sweeper"; }
    if spe >= 100 && atk >= 105 && atk >= spa { return "Physical Sweeper"; }
    if atk >= 105 && spa >= 105 { return "Mixed Wallbreaker"; }
    if def >= 110 && hp >= 85 { return "Physical Wall"; }
    if spd >= 110 && hp >= 85 { return "Special Wall"; }
    if atk >= 115 { return "Physical Heavyweight"; }
    if spa >= 115 { return "Special Heavyweight"; }
    if hp + def + spd >= 280 { return "Bulky Bruiser"; }
    "Balanced All-Rounder"
}

pub fn hp_bar(curr: i32, max_hp: i32, width: usize) -> String {
    let pct = (curr.max(0) as f32 / max_hp.max(1) as f32).clamp(0.0, 1.0);
    let filled = (pct * width as f32).round() as usize;
    let (r, g, b) = if pct > 0.5 { (85, 235, 115) } else if pct > 0.25 { (255, 210, 65) } else { (255, 75, 75) };
    format!("[{}{}]", "█".repeat(filled).truecolor(r, g, b), "░".repeat(width - filled).truecolor(55, 55, 65))
}

fn stat_bar_colored(val: i32, bar_len: usize) -> String {
    let filled = ((val as f32 / 255.0) * bar_len as f32).round().min(bar_len as f32) as usize;
    let (r, g, b) = if val >= 120 { (80, 240, 255) } else if val >= 95 { (90, 235, 115) } else if val >= 65 { (255, 210, 75) } else { (255, 95, 95) };
    format!("{}{}", "█".repeat(filled).truecolor(r, g, b), "░".repeat(bar_len - filled).truecolor(65, 65, 75))
}

fn stat_tier_tag(val: i32) -> (&'static str, Rgb) {
    if val >= 130 { ("[MYTHIC]", (80, 240, 255)) }
    else if val >= 100 { ("[ELITE]", (90, 235, 115)) }
    else if val >= 75 { ("[SOLID]", (255, 210, 75)) }
    else if val >= 50 { ("[AVG]", (220, 155, 90)) }
    else { ("[LOW]", (255, 95, 95)) }
}

fn format_evo_trigger(details: &Value) -> String {
    let Some(d) = details.as_array().and_then(|a| a.first()) else { return String::new(); };
    let mut parts = Vec::new();
    if let Some(lvl) = d["min_level"].as_i64() { parts.push(format!("Lv. {}", lvl)); }
    if let Some(item) = d["item"]["name"].as_str() { parts.push(item.replace('-', " ").capitalize()); }
    if let Some(held) = d["held_item"]["name"].as_str() { parts.push(format!("Hold {}", held.replace('-', " ").capitalize())); }
    if let Some(hap) = d["min_happiness"].as_i64() { parts.push(format!("Friendship {}", hap)); }
    if let Some(tod) = d["time_of_day"].as_str().filter(|s| !s.is_empty()) { parts.push(tod.capitalize()); }
    if let Some(mt) = d["known_move_type"]["name"].as_str() { parts.push(format!("{} Move", mt.capitalize())); }
    if let Some(trig) = d["trigger"]["name"].as_str() {
        if trig == "trade" { parts.push("Trade".to_string()); }
    }
    if parts.is_empty() { "Special".to_string() } else { parts.join(" + ") }
}

fn is_branching_chain(node: &Value) -> bool {
    if let Some(children) = node["evolves_to"].as_array() {
        if children.len() > 1 { return true; }
        return children.iter().any(is_branching_chain);
    }
    false
}

fn collect_evo_tree(node: &Value, prefix: &str, is_last: bool, is_root: bool, lines: &mut Vec<String>, names: &mut Vec<String>) {
    let name = node["species"]["name"].as_str().unwrap_or("Unknown").capitalize();
    names.push(name.clone());
    if is_root {
        lines.push(name);
    } else {
        let branch = if is_last { "└── " } else { "├── " };
        let trig = format_evo_trigger(&node["evolution_details"]);
        let trig_s = if trig.is_empty() { String::new() } else { format!(" [{}]", trig) };
        lines.push(format!("{}{}{}{}", prefix, branch, name, trig_s));
    }
    if let Some(children) = node["evolves_to"].as_array() {
        let next_prefix = if is_root { String::new() } else { format!("{}{}", prefix, if is_last { "    " } else { "│   " }) };
        for (idx, child) in children.iter().enumerate() {
            collect_evo_tree(child, &next_prefix, idx + 1 == children.len(), false, lines, names);
        }
    }
}

fn build_evolution_data(chain: &Value, fallback: &str) -> (String, Vec<String>) {
    if is_branching_chain(chain) {
        let (mut lines, mut names) = (Vec::new(), Vec::new());
        collect_evo_tree(chain, "", true, true, &mut lines, &mut names);
        return (names.join(" -> "), lines);
    }
    let mut parts = Vec::new();
    let mut curr = Some(chain);
    let mut is_first = true;
    while let Some(c) = curr {
        if let Some(sn) = c["species"]["name"].as_str() {
            let nm = sn.capitalize();
            if is_first {
                parts.push(nm);
            } else {
                let trig = format_evo_trigger(&c["evolution_details"]);
                if trig.is_empty() { parts.push(nm); } else { parts.push(format!("{} [{}]", nm, trig)); }
            }
            is_first = false;
        }
        curr = c["evolves_to"].as_array().and_then(|a| a.first());
    }
    let flat = if parts.is_empty() { fallback.to_string() } else { parts.join(" -> ") };
    (flat.clone(), vec![flat])
}

fn clean_wikitext(raw: &str) -> String {
    let re_ref_pair = Regex::new(r"(?s)<ref[^>]*>.*?</ref>").unwrap();
    let re_ref_single = Regex::new(r"<ref[^>]*/>").unwrap();
    let re_comment = Regex::new(r"(?s)<!--.*?-->").unwrap();
    let re_file = Regex::new(r"\[\[(?:File|Image):[^\]]+\]\]").unwrap();
    let re_inline_tpl = Regex::new(r"\{\{(?:p|m|t|a|type|egg|status|cat|wp|pkmn|v2|game|an|obp|tt)\|([^|\}]+)(?:\|[^\}\{]*)?\}\}").unwrap();
    let re_any_tpl = Regex::new(r"\{\{[^{}]*\}\}").unwrap();
    let re_link_pipe = Regex::new(r"\[\[[^|\]]+\|([^\]]+)\]\]").unwrap();
    let re_link_plain = Regex::new(r"\[\[([^\]]+)\]\]").unwrap();
    let re_html = Regex::new(r"<[^>]+>").unwrap();

    let mut s = re_ref_pair.replace_all(raw, "").to_string();
    s = re_ref_single.replace_all(&s, "").to_string();
    s = re_comment.replace_all(&s, "").to_string();
    s = re_file.replace_all(&s, "").to_string();
    for _ in 0..3 {
        s = re_inline_tpl.replace_all(&s, "$1").to_string();
        s = re_any_tpl.replace_all(&s, "").to_string();
    }
    s = re_link_pipe.replace_all(&s, "$1").to_string();
    s = re_link_plain.replace_all(&s, "$1").to_string();
    s = re_html.replace_all(&s, "").to_string();
    s.replace("'''", "").replace("''", "").replace("&rarr;", "->").replace("&mdash;", "—").replace("&ndash;", "-").replace("&nbsp;", " ").replace("&amp;", "&")
        .split_whitespace().collect::<Vec<_>>().join(" ")
}

struct BulbapediaData {
    anime_moves: Vec<String>,
    manga_moves: Vec<String>,
    trivia_and_lore: Vec<String>,
    anime_manga_lore: Vec<String>,
}

fn fetch_bulbapedia(pokemon_name: &str) -> BulbapediaData {
    let (mut anime_moves, mut manga_moves) = (BTreeSet::new(), BTreeSet::new());
    let (mut trivia_and_lore, mut anime_manga_lore) = (Vec::new(), Vec::new());

    let page = pokemon_name.split('-').map(|p| p.capitalize()).collect::<Vec<_>>().join("_");
    let url = format!("https://bulbapedia.bulbagarden.net/w/api.php?action=parse&page={}_(Pok%C3%A9mon)&prop=wikitext&format=json&redirects=1", page);

    let Ok(resp) = ureq::get(&url).set("User-Agent", "pokefact/1.0").call() else {
        return BulbapediaData { anime_moves: vec![], manga_moves: vec![], trivia_and_lore: vec![], anime_manga_lore: vec![] };
    };
    let Ok(json) = resp.into_json::<Value>() else {
        return BulbapediaData { anime_moves: vec![], manga_moves: vec![], trivia_and_lore: vec![], anime_manga_lore: vec![] };
    };

    let wikitext = json["parse"]["wikitext"]["*"].as_str().unwrap_or("");
    let re_move_tpl = Regex::new(r"\{\{m\|([^|\}]+)").unwrap();
    let mut sec = String::new();
    let mut in_table = false;

    for raw_line in wikitext.lines() {
        let t = raw_line.trim();
        if t.starts_with("{|") { in_table = true; continue; }
        if t.starts_with("|}") { in_table = false; continue; }
        if in_table { continue; }
        if t.starts_with("==") && t.ends_with("==") {
            sec = t.trim_matches('=').trim().to_lowercase();
            continue;
        }
        let is_anime = sec.contains("animation") || sec.contains("anime");
        let is_manga = sec.contains("manga");
        let is_lore = matches!(sec.as_str(), "biology" | "trivia" | "origin" | "name origin");

        if is_anime {
            for cap in re_move_tpl.captures_iter(t) { if cap[1].trim().len() > 1 { anime_moves.insert(cap[1].trim().to_string()); } }
        } else if is_manga {
            for cap in re_move_tpl.captures_iter(t) { if cap[1].trim().len() > 1 { manga_moves.insert(cap[1].trim().to_string()); } }
        }

        if t.is_empty() || t.starts_with('|') || t.starts_with('!') || t.starts_with("[[File:") || t.starts_with("[[Image:") || t.starts_with("{{") || t.contains("style=") || t.contains("px|") || t.contains("thumb|") {
            continue;
        }
        if is_lore && (t.starts_with('*') || t.len() > 45) {
            let cleaned = clean_wikitext(t.trim_start_matches('*').trim());
            if cleaned.len() > 30 && !cleaned.contains("refer to Game data") && !cleaned.contains("Pokédex entries") {
                let tag = match sec.as_str() { "biology" => "Biology", "trivia" => "Trivia", "origin" | "name origin" => "Origin", _ => "Lore" };
                trivia_and_lore.push(format!("[{}] {}", tag, cleaned));
            }
        } else if (is_anime || is_manga) && t.len() > 50 && anime_manga_lore.len() < 8 {
            let cleaned = clean_wikitext(t.trim_start_matches('*').trim());
            if cleaned.len() > 35 && !cleaned.starts_with("Main article") {
                anime_manga_lore.push(format!("[{}] {}", if is_anime { "Anime" } else { "Manga" }, cleaned));
            }
        }
    }

    BulbapediaData {
        anime_moves: anime_moves.into_iter().collect(),
        manga_moves: manga_moves.into_iter().collect(),
        trivia_and_lore,
        anime_manga_lore,
    }
}

fn fan_theories(name_lower: &str) -> Vec<&'static str> {
    match name_lower {
        "gengar" => vec!["SHADOW OF CLEFABLE: Gengar mirrors Clefable's silhouette, ears, and spikes, leading to the theory that it is Clefable's shadow."],
        "cubone" | "marowak" => vec!["ORPHANED KANGASKHAN: Cubone's shape and skull closely resemble a baby Kangaskhan wearing its mother's skull."],
        "ditto" => vec!["FAILED MEW CLONE: Ditto shares Mew's 4.0kg weight, color palettes, and exclusive natural access to Transform."],
        "parasect" => vec!["CORDYCEPS HOST: The bug host underneath is dead; the mushroom on its back controls the body."],
        "mimikyu" => vec!["DISGUISED SPIRIT: A scholar who looked beneath its Pikachu rag died of shock."],
        "arceus" => vec!["AVATAR THEORY: The Arceus caught in a Poke Ball is only one fragment lent to observe humanity."],
        _ => vec![],
    }
}

pub fn fetch_pokemon(query: &str, force_refresh: bool) -> Option<Value> {
    let mut key = query.trim().to_lowercase();
    if !force_refresh {
        if let Some(cached) = load_cache(&key) {
            if cached.get("evo_lines").is_some() { return Some(cached); }
        }
    }

    let species_url = format!("https://pokeapi.co/api/v2/pokemon-species/{}", key);
    let resp = match ureq::get(&species_url).call() {
        Ok(r) => r,
        Err(ureq::Error::Status(404, _)) => {
            if let Some(fixed) = autocorrect_name(&key) {
                println!("  {}", format!("Did you mean '{}'? Loading it...", fixed).yellow().bold());
                key = fixed;
                if !force_refresh {
                    if let Some(cached) = load_cache(&key) {
                        if cached.get("evo_lines").is_some() { return Some(cached); }
                    }
                }
                ureq::get(&format!("https://pokeapi.co/api/v2/pokemon-species/{}", key)).call().ok()?
            } else {
                return None;
            }
        }
        Err(e) => {
            eprintln!("\n  Network error: {}", e);
            return None;
        }
    };

    let species_json: Value = resp.into_json().ok()?;
    let raw_name = species_json["name"].as_str()?.to_string();
    let name = raw_name.to_uppercase();
    let id = species_json["id"].as_i64().unwrap_or(0);
    let genus = species_json["genera"].as_array()
        .and_then(|arr| arr.iter().find(|g| g["language"]["name"].as_str() == Some("en")))
        .and_then(|g| g["genus"].as_str()).unwrap_or("Unknown Pokemon").to_string();
    let generation = species_json["generation"]["name"].as_str().unwrap_or("unknown").replace("generation-", "Gen ").to_uppercase();
    let capture_rate = species_json["capture_rate"].as_i64().unwrap_or(0);

    let varieties = species_json["varieties"].as_array()?;
    let default_var_url = varieties.iter().find(|v| v["is_default"].as_bool().unwrap_or(false))
        .and_then(|v| v["pokemon"]["url"].as_str())
        .unwrap_or_else(|| varieties[0]["pokemon"]["url"].as_str().unwrap()).to_string();
    let evo_url = species_json["evolution_chain"]["url"].as_str().map(String::from);

    let (pokemon_resp_opt, bulba, (evo_chain_str, evo_lines)) = thread::scope(|s| {
        let h_poke = s.spawn(|| ureq::get(&default_var_url).call().ok().and_then(|r| r.into_json::<Value>().ok()));
        let h_bulba = s.spawn(|| fetch_bulbapedia(&raw_name));
        let h_evo = s.spawn(|| {
            if let Some(u) = evo_url {
                if let Some(evo_resp) = ureq::get(&u).call().ok().and_then(|r| r.into_json::<Value>().ok()) {
                    return build_evolution_data(&evo_resp["chain"], &raw_name.capitalize());
                }
            }
            let fb = raw_name.capitalize();
            (fb.clone(), vec![fb])
        });
        (
            h_poke.join().ok().flatten(),
            h_bulba.join().unwrap_or(BulbapediaData { anime_moves: vec![], manga_moves: vec![], trivia_and_lore: vec![], anime_manga_lore: vec![] }),
            h_evo.join().unwrap_or_else(|_| { let fb = raw_name.capitalize(); (fb.clone(), vec![fb]) }),
        )
    });

    let pokemon_resp = pokemon_resp_opt?;
    let height = pokemon_resp["height"].as_f64().unwrap_or(0.0) / 10.0;
    let weight = pokemon_resp["weight"].as_f64().unwrap_or(0.0) / 10.0;
    let cry_url = pokemon_resp["cries"]["latest"].as_str().or_else(|| pokemon_resp["cries"]["legacy"].as_str()).unwrap_or("").to_string();

    let types: Vec<String> = pokemon_resp["types"].as_array().unwrap_or(&vec![]).iter().filter_map(|t| t["type"]["name"].as_str().map(|s| s.capitalize())).collect();
    let abilities: Vec<String> = pokemon_resp["abilities"].as_array().unwrap_or(&vec![]).iter().filter_map(|a| {
        let n = a["ability"]["name"].as_str()?.split('-').map(|w| w.capitalize()).collect::<Vec<_>>().join(" ");
        Some(if a["is_hidden"].as_bool().unwrap_or(false) { format!("{} (Hidden)", n) } else { n })
    }).collect();
    let stats: Vec<(String, i32)> = pokemon_resp["stats"].as_array().unwrap_or(&vec![]).iter().filter_map(|s| {
        let sn = match s["stat"]["name"].as_str()? {
            "hp" => "HP".into(), "attack" => "Attack".into(), "defense" => "Defense".into(),
            "special-attack" => "Sp. Atk".into(), "special-defense" => "Sp. Def".into(), "speed" => "Speed".into(),
            other => other.replace('-', " ").capitalize(),
        };
        Some((sn, s["base_stat"].as_i64()? as i32))
    }).collect();

    let (mut level_moves, mut tm_moves, mut tutor_moves, mut egg_moves) = (Vec::new(), BTreeSet::new(), BTreeSet::new(), BTreeSet::new());
    if let Some(moves_arr) = pokemon_resp["moves"].as_array() {
        for m in moves_arr {
            if let Some(m_name) = m["move"]["name"].as_str() {
                let clean = m_name.split('-').map(|p| p.capitalize()).collect::<Vec<_>>().join(" ");
                if let Some(details) = m["version_group_details"].as_array() {
                    let mut best_lvl = None;
                    for d in details {
                        match d["move_learn_method"]["name"].as_str() {
                            Some("level-up") => best_lvl = Some(d["level_learned_at"].as_i64().unwrap_or(0)),
                            Some("machine") => { tm_moves.insert(clean.clone()); }
                            Some("tutor") => { tutor_moves.insert(clean.clone()); }
                            Some("egg") => { egg_moves.insert(clean.clone()); }
                            _ => {}
                        }
                    }
                    if let Some(lvl) = best_lvl {
                        if !level_moves.iter().any(|(_, n)| n == &clean) { level_moves.push((lvl, clean)); }
                    }
                }
            }
        }
    }
    level_moves.sort_by_key(|(lvl, _)| *lvl);

    let sprites = &pokemon_resp["sprites"];
    let normal_sprite = sprites["front_default"].as_str().unwrap_or("").to_string();
    let shiny_sprite = sprites["front_shiny"].as_str().unwrap_or("").to_string();
    let hd_artwork = sprites["other"]["official-artwork"]["front_default"].as_str().unwrap_or("").to_string();
    let animated_sprite = sprites["versions"]["generation-v"]["black-white"]["animated"]["front_default"].as_str().unwrap_or("").to_string();

    let (mut pokedex_entries, mut raw_dex_short, mut seen_norm) = (Vec::new(), Vec::new(), Vec::new());
    if let Some(entries) = species_json["flavor_text_entries"].as_array() {
        for entry in entries {
            if entry["language"]["name"].as_str() == Some("en") {
                let text = entry["flavor_text"].as_str().unwrap_or("").replace(['\n', '\r', '\x0c', '\u{00ad}'], " ").split_whitespace().collect::<Vec<_>>().join(" ");
                let norm: String = text.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect();
                if !text.is_empty() && !seen_norm.contains(&norm) {
                    seen_norm.push(norm);
                    let ver = entry["version"]["name"].as_str().unwrap_or("").replace('-', " ").capitalize();
                    pokedex_entries.push(format!("[Pokedex - {}] {}", ver, text));
                    if raw_dex_short.len() < 3 { raw_dex_short.push(text); }
                }
            }
        }
    }

    let mut combined_lore = Vec::new();
    for i in 0..bulba.trivia_and_lore.len().max(pokedex_entries.len()) {
        if i < bulba.trivia_and_lore.len() { combined_lore.push(bulba.trivia_and_lore[i].clone()); }
        if i < pokedex_entries.len() { combined_lore.push(pokedex_entries[i].clone()); }
    }

    let form_urls: Vec<(String, String)> = varieties.iter()
        .filter(|v| !v["is_default"].as_bool().unwrap_or(false)).take(10)
        .filter_map(|v| Some((v["pokemon"]["name"].as_str()?.to_string(), v["pokemon"]["url"].as_str()?.to_string())))
        .collect();
    let mut alt_forms = serde_json::Map::new();
    let fetched_forms: Vec<(String, String)> = thread::scope(|s| {
        let hs: Vec<_> = form_urls.into_iter().map(|(vn, vu)| s.spawn(move || {
            let vj = ureq::get(&vu).call().ok()?.into_json::<Value>().ok()?;
            Some((vn, vj["sprites"]["front_default"].as_str()?.to_string()))
        })).collect();
        hs.into_iter().filter_map(|h| h.join().ok().flatten()).collect()
    });
    for (vn, sp) in fetched_forms { alt_forms.insert(vn, Value::String(sp)); }

    let compiled = serde_json::json!({
        "id": id, "name": name, "genus": genus, "generation": generation, "capture_rate": capture_rate,
        "height": height, "weight": weight, "cry_url": cry_url, "types": types, "abilities": abilities,
        "stats": stats, "level_moves": level_moves,
        "tm_moves": tm_moves.into_iter().collect::<Vec<_>>(),
        "tutor_moves": tutor_moves.into_iter().collect::<Vec<_>>(),
        "egg_moves": egg_moves.into_iter().collect::<Vec<_>>(),
        "anime_moves": bulba.anime_moves, "manga_moves": bulba.manga_moves, "anime_manga_lore": bulba.anime_manga_lore,
        "normal_sprite": normal_sprite, "shiny_sprite": shiny_sprite, "hd_artwork": hd_artwork, "animated_sprite": animated_sprite,
        "lore": combined_lore, "quiz_clues": raw_dex_short, "evolution": evo_chain_str, "evo_lines": evo_lines, "alt_forms": alt_forms
    });

    save_cache(&key, &compiled);
    save_cache(&id.to_string(), &compiled);
    save_cache(&raw_name.to_lowercase(), &compiled);
    Some(compiled)
}

fn print_box_line(content: &str, theme: Theme) {
    let b = "│".truecolor(theme.primary.0, theme.primary.1, theme.primary.2);
    println!("  {} {} {}", b, content, b);
}

fn print_wrapped_box_row(label: &str, value: &str, l_rgb: Rgb, v_rgb: Rgb, content_w: usize, theme: Theme) {
    let val_w = content_w.saturating_sub(11);
    for (idx, line) in wrap_text(value, val_w).iter().enumerate() {
        let l = pad_right(if idx == 0 { label } else { "" }, 11).truecolor(l_rgb.0, l_rgb.1, l_rgb.2).bold();
        let v = pad_right(line, val_w).truecolor(v_rgb.0, v_rgb.1, v_rgb.2);
        print_box_line(&format!("{}{}", l, v), theme);
    }
}

fn print_move_block(title: &str, moves: &[String], box_w: usize, accent: Rgb) {
    if moves.is_empty() { return; }
    println!("\n  {}", format!("{} ({})", title, moves.len()).truecolor(accent.0, accent.1, accent.2).bold());
    for line in wrap_text(&moves.join(", "), box_w) { println!("  {}", line.truecolor(215, 220, 230)); }
}

pub fn display_pokemon(data: &Value, is_wild_shiny: bool, max_facts: usize, play_audio: bool, hd_mode: bool) {
    let name = data["name"].as_str().unwrap_or("UNKNOWN");
    let id = data["id"].as_i64().unwrap_or(0);
    let audio_handle = if play_audio { spawn_play_cry(id, name.to_string(), data["cry_url"].as_str().unwrap_or("").to_string()) } else { None };

    let types: Vec<String> = data["types"].as_array().unwrap_or(&vec![]).iter().filter_map(|t| t.as_str().map(String::from)).collect();
    let abilities: Vec<String> = data["abilities"].as_array().unwrap_or(&vec![]).iter().filter_map(|a| a.as_str().map(String::from)).collect();
    let stats: Vec<(String, i32)> = data["stats"].as_array().unwrap_or(&vec![]).iter().filter_map(|s| Some((s[0].as_str()?.to_string(), s[1].as_i64()? as i32))).collect();

    let theme = if is_wild_shiny { Theme { primary: (255, 215, 70), secondary: (255, 120, 220) } } else { theme_for_types(&types) };
    let tw = term_width();
    let box_inner = tw.saturating_sub(5).clamp(30, 102);

    println!();
    let audio_tag = if play_audio { " [AUDIO]" } else { "" };
    let full_title = format!(" #{:04} {} - {} [{}]{} ", id, name, data["genus"].as_str().unwrap_or(""), data["generation"].as_str().unwrap_or(""), audio_tag);
    let short_title = format!(" #{:04} {} [{}]{} ", id, name, data["generation"].as_str().unwrap_or(""), audio_tag);
    let max_banner_w = box_inner + 2;
    let title_str = if full_title.chars().count() <= max_banner_w {
        full_title
    } else if short_title.chars().count() <= max_banner_w {
        short_title
    } else {
        short_title.chars().take(max_banner_w).collect()
    };
    let rem = max_banner_w.saturating_sub(title_str.chars().count());
    let left_bar = rem / 2;
    let right_bar = rem - left_bar;
    let banner = format!("{}{}{}", "━".repeat(left_bar), title_str, "━".repeat(right_bar));
    println!("  {}\n", banner.truecolor(theme.primary.0, theme.primary.1, theme.primary.2).bold());

    if hd_mode {
        graphics::render_hd_artwork(data);
    } else {
        let sprite_max_w = (tw.saturating_sub(6) as u32).clamp(24, 56);
        let mut jobs = Vec::new();
        if let Some(u) = data["normal_sprite"].as_str().filter(|s| !s.is_empty()) { jobs.push(("NORMAL".to_string(), u.to_string())); }
        if let Some(u) = data["shiny_sprite"].as_str().filter(|s| !s.is_empty()) { jobs.push(("SHINY".to_string(), u.to_string())); }
        if let Some(forms) = data["alt_forms"].as_object() {
            for (fnm, fu) in forms {
                if let Some(u) = fu.as_str().filter(|s| !s.is_empty()) { jobs.push((fnm.replace('-', " ").to_uppercase(), u.to_string())); }
            }
        }
        let sprites: Vec<(String, usize, Vec<String>)> = thread::scope(|s| {
            let hs: Vec<_> = jobs.into_iter().map(|(l, u)| s.spawn(move || fetch_bytes(&u).map(|b| { let (w, ln) = graphics::render_sprite_lines(&b, sprite_max_w, false); (l, w, ln) }))).collect();
            hs.into_iter().filter_map(|h| h.join().ok().flatten()).collect()
        });
        graphics::print_sprite_grid(&sprites, theme);
    }

    let content_w = box_inner - 2;
    println!("{}", format!("  ╭{}╮", "─".repeat(box_inner)).truecolor(theme.primary.0, theme.primary.1, theme.primary.2));
    print_wrapped_box_row("TYPE", &types.join(" / "), theme.primary, theme.secondary, content_w, theme);
    print_wrapped_box_row("ROLE", detect_archetype(&stats), theme.primary, theme.primary, content_w, theme);
    print_wrapped_box_row("ABILITIES", &abilities.join(", "), theme.primary, (235, 235, 240), content_w, theme);

    let evo_lines: Vec<String> = data["evo_lines"]
        .as_array()
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();

    if evo_lines.len() > 1 {
        let val_w = content_w.saturating_sub(11);
        for (idx, line) in evo_lines.iter().enumerate() {
            let l = pad_right(if idx == 0 { "EVOLUTION" } else { "" }, 11).truecolor(theme.primary.0, theme.primary.1, theme.primary.2).bold();
            let v = pad_right(line, val_w).truecolor(theme.secondary.0, theme.secondary.1, theme.secondary.2);
            print_box_line(&format!("{}{}", l, v), theme);
        }
    } else {
        print_wrapped_box_row("EVOLUTION", data["evolution"].as_str().unwrap_or(""), theme.primary, theme.secondary, content_w, theme);
    }

    print_wrapped_box_row("SIZE/CATCH", &format!("{}m / {}kg | Catch: {}/255", data["height"].as_f64().unwrap_or(0.0), data["weight"].as_f64().unwrap_or(0.0), data["capture_rate"].as_i64().unwrap_or(0)), theme.primary, (210, 215, 225), content_w, theme);

    let (x4, x2, h, q, imm) = type_matchups(&types);
    println!("{}", format!("  ├{}┤", "─".repeat(box_inner)).truecolor(theme.primary.0, theme.primary.1, theme.primary.2));
    if !x4.is_empty() { print_wrapped_box_row("4x WEAK", &x4.join(", "), (255, 70, 70), (255, 110, 110), content_w, theme); }
    if !x2.is_empty() { print_wrapped_box_row("2x WEAK", &x2.join(", "), (255, 145, 80), (255, 180, 120), content_w, theme); }
    if !imm.is_empty() { print_wrapped_box_row("0x IMMUNE", &imm.join(", "), (120, 235, 255), (160, 245, 255), content_w, theme); }
    let mut resists: Vec<String> = q.into_iter().map(|r| format!("{} (0.25x)", r)).collect();
    resists.extend(h);
    if !resists.is_empty() { print_wrapped_box_row("RESIST", &resists.join(", "), (95, 230, 130), (145, 235, 170), content_w, theme); }

    println!("{}", format!("  ├{}┤", "─".repeat(box_inner)).truecolor(theme.primary.0, theme.primary.1, theme.primary.2));
    let show_tag = content_w >= 34;
    let bar_w = if show_tag { content_w.saturating_sub(23) } else { content_w.saturating_sub(14) };
    let mut bst = 0;
    for (s_name, val) in &stats {
        bst += *val;
        let (tag_str, tag_rgb) = stat_tier_tag(*val);
        let row = if show_tag {
            format!(
                "{} {} {} {}",
                pad_right(s_name, 9).truecolor(theme.primary.0, theme.primary.1, theme.primary.2),
                format!("{:>3}", val).truecolor(255, 230, 120).bold(),
                stat_bar_colored(*val, bar_w),
                pad_right(tag_str, 8).truecolor(tag_rgb.0, tag_rgb.1, tag_rgb.2)
            )
        } else {
            format!(
                "{} {} {}",
                pad_right(s_name, 9).truecolor(theme.primary.0, theme.primary.1, theme.primary.2),
                format!("{:>3}", val).truecolor(255, 230, 120).bold(),
                stat_bar_colored(*val, bar_w)
            )
        };
        print_box_line(&row, theme);
    }
    print_box_line(&pad_right(&format!("BASE STAT TOTAL (BST): {}", bst), content_w).truecolor(255, 255, 255).bold().to_string(), theme);
    println!("{}", format!("  ╰{}╯", "─".repeat(box_inner)).truecolor(theme.primary.0, theme.primary.1, theme.primary.2));

    if let Some(moves) = data["level_moves"].as_array() {
        if !moves.is_empty() {
            println!("\n  {}", format!("NATURAL LEVEL-UP ATTACKS ({})", moves.len()).truecolor(theme.primary.0, theme.primary.1, theme.primary.2).bold());
            let cols = (box_inner / 24).clamp(1, 4);
            for chunk in moves.chunks(cols) {
                let mut row_s = String::from("  ");
                for (i, m) in chunk.iter().enumerate() {
                    row_s.push_str(&format!("L{:02} {}", m[0].as_i64().unwrap_or(0), pad_right(m[1].as_str().unwrap_or(""), 17)));
                    if i + 1 < chunk.len() { row_s.push_str(" │ "); }
                }
                println!("{}", row_s.truecolor(220, 225, 235));
            }
        }
    }

    let extract = |k: &str| -> Vec<String> { data[k].as_array().unwrap_or(&vec![]).iter().filter_map(|v| v.as_str().map(String::from)).collect() };
    print_move_block("ANIME & MOVIE ATTACKS", &extract("anime_moves"), box_inner, (255, 210, 80));
    print_move_block("MANGA ATTACKS", &extract("manga_moves"), box_inner, (255, 130, 210));
    print_move_block("TM / HM / TR ATTACKS", &extract("tm_moves"), box_inner, theme.secondary);
    print_move_block("MOVE TUTOR ATTACKS", &extract("tutor_moves"), box_inner, (110, 220, 255));
    print_move_block("EGG / BREEDING ATTACKS", &extract("egg_moves"), box_inner, (130, 235, 150));

    let theories = fan_theories(&name.to_lowercase());
    if !theories.is_empty() {
        println!("\n  {}", "FAN THEORIES & LORE".truecolor(255, 110, 175).bold());
        for (i, th) in theories.iter().enumerate() {
            for (wi, wl) in wrap_text(th, box_inner.saturating_sub(4)).iter().enumerate() {
                if wi == 0 { println!("  {}. {}", (i + 1).to_string().truecolor(255, 110, 175).bold(), wl.italic()); }
                else { println!("     {}", wl.italic()); }
            }
        }
    }

    println!("\n  {}", "POKEDEX LORE, BIOLOGY & TRIVIA".truecolor(theme.primary.0, theme.primary.1, theme.primary.2).bold());
    if let Some(lore) = data["lore"].as_array() {
        for (i, l) in lore.iter().take(max_facts).enumerate() {
            if let Some(text) = l.as_str() {
                for (wi, wl) in wrap_text(text, box_inner.saturating_sub(4)).iter().enumerate() {
                    if wi == 0 { println!("  {:>2}. {}", (i + 1).to_string().truecolor(theme.secondary.0, theme.secondary.1, theme.secondary.2).bold(), wl); }
                    else { println!("      {}", wl); }
                }
            }
        }
    }
    println!();
    if let Some(h) = audio_handle { let _ = h.join(); }
}

pub fn rand_seed() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    d.subsec_nanos() ^ (d.as_secs() as u32)
}

pub trait Capitalize { fn capitalize(&self) -> String; }
impl Capitalize for str {
    fn capitalize(&self) -> String {
        let mut c = self.chars();
        match c.next() {
            None => String::new(),
            Some(f) => f.to_uppercase().collect::<String>() + &c.as_str().to_lowercase(),
        }
    }
}

fn print_help() {
    println!("\n  {}", "pokefact - usage".bright_cyan().bold());
    println!("  pokefact -p <name> [-a] [--hd] [--export]   Pokedex lookup (fuzzy typo matching)");
    println!("  pokefact -i <index> [-a]                    Lookup by National Dex number");
    println!("  pokefact --fight <p1> <p2>                  Interactive 4-move turn-based battle");
    println!("  pokefact <p1> --vs <p2>                     Auto-simulated Battle Arena");
    println!("  pokefact --calc <p1> \"<move>\" <p2>          Lv.50 & Lv.100 damage calculator");
    println!("  pokefact --type <t1> [t2]                   Dual-type weakness/immunity chart");
    println!("  pokefact -p <name> --animate [--infi]       Animated sprite (10s, or --infi)");
    println!("  pokefact --fetch [name]                     System info fetch with sprite");
    println!("  pokefact --banner [name]                    Compact shell startup banner");
    println!("  pokefact --daily [-a] [--hd]                Pokemon of the Day + streak");
    println!("  pokefact --quiz [-a]                        Silhouette quiz with fuzzy matching");
    println!("  pokefact -p <name> --export[=PATH]          Save shareable PNG card");
    println!("  pokefact -p <name> --catch / --box          Catch into and view PC Box\n");
}

fn main() {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("cmd").args(["/C", "chcp 65001 > nul"]).status();
        let _ = colored::control::set_virtual_terminal(true);
    }

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") { print_help(); return; }

    let play_audio = args.iter().any(|a| a == "-a" || a == "--audio");
    let force_refresh = args.iter().any(|a| a == "--refresh" || a == "-r");
    let do_catch = args.iter().any(|a| a == "--catch");
    let show_all = args.iter().any(|a| a == "--all");
    let hd_mode = args.iter().any(|a| a == "--hd");
    let do_infi = args.iter().any(|a| a == "--infi");
    let do_animate = do_infi || args.iter().any(|a| a == "--animate");

    let mut do_export = false;
    let mut export_dest: Option<String> = None;
    for a in &args {
        if a == "--export" {
            do_export = true;
        } else if let Some(val) = a.strip_prefix("--export=") {
            do_export = true;
            if !val.is_empty() { export_dest = Some(val.to_string()); }
        }
    }

    if args.iter().any(|a| a == "--quiz" || a == "-q") { modes::run_quiz_mode(play_audio); return; }
    if args.iter().any(|a| a == "--box" || a == "-b") { modes::show_box(); return; }
    if args.iter().any(|a| a == "--daily") { modes::run_daily_mode(play_audio, hd_mode); return; }

    if let Some(pos) = args.iter().position(|a| a == "--fight") {
        if pos + 2 < args.len() { modes::run_fight_mode(&args[pos + 1], &args[pos + 2]); return; }
        eprintln!("  {}", "Usage: pokefact --fight <pokemon1> <pokemon2>".red()); return;
    }
    if let Some(pos) = args.iter().position(|a| a == "--calc") {
        if pos + 3 < args.len() { modes::run_calc_mode(&args[pos + 1], &args[pos + 2], &args[pos + 3]); return; }
        eprintln!("  {}", "Usage: pokefact --calc gengar \"shadow ball\" lucario".red()); return;
    }
    if let Some(pos) = args.iter().position(|a| a == "--type") {
        let t_args: Vec<String> = args[pos + 1..].iter().take_while(|a| !a.starts_with('-')).cloned().collect();
        modes::run_type_sandbox(&t_args);
        return;
    }
    if let Some(pos) = args.iter().position(|a| a == "--fetch") {
        let opt = args.get(pos + 1).filter(|a| !a.starts_with('-')).cloned();
        modes::run_fetch_mode(opt);
        return;
    }
    if let Some(pos) = args.iter().position(|a| a == "--banner") {
        let opt = args.get(pos + 1).filter(|a| !a.starts_with('-')).cloned();
        modes::run_banner_mode(opt);
        return;
    }
    if let Some(pos) = args.iter().position(|a| a == "--vs") {
        if pos > 0 && pos + 1 < args.len() { modes::run_vs_mode(&args[pos - 1], &args[pos + 1], play_audio); return; }
    }

    let mut query_opt: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-i" | "--id" => {
                if let Some(v) = args.get(i + 1) {
                    if v.chars().all(|c| c.is_ascii_digit()) { query_opt = Some(v.clone()); }
                    else { eprintln!("  -i expects a numeric National Dex ID."); std::process::exit(1); }
                    i += 2; continue;
                }
            }
            "-p" | "--pokemon" => {
                if let Some(v) = args.get(i + 1) { query_opt = Some(v.clone()); i += 2; continue; }
            }
            s if s.starts_with('-') => {}
            other => { if query_opt.is_none() { query_opt = Some(other.to_string()); } }
        }
        i += 1;
    }

    let is_random = query_opt.is_none();
    let query = query_opt.unwrap_or_else(|| ((rand_seed() % 1025) + 1).to_string());
    if query.eq_ignore_ascii_case("missingno") || query == "0" { modes::trigger_missingno(); return; }

    let pb = ProgressBar::new_spinner();
    pb.set_style(ProgressStyle::default_spinner().tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏").template("{spinner:.cyan} Fetching '{msg}'...").unwrap());
    pb.set_message(query.clone());
    pb.enable_steady_tick(std::time::Duration::from_millis(80));

    let Some(data) = fetch_pokemon(&query, force_refresh) else {
        pb.finish_and_clear();
        eprintln!("  Pokemon '{}' not found.", query);
        std::process::exit(1);
    };
    pb.finish_and_clear();

    if do_animate {
        graphics::play_animated_sprite(&data, do_infi);
    } else {
        display_pokemon(&data, is_random && (rand_seed() % 25 == 0), if show_all { 200 } else { 25 }, play_audio, hd_mode);
    }

    if do_export { graphics::export_pokemon_card(&data, export_dest.as_deref()); }
    if do_catch { modes::catch_pokemon(&data); }
}
