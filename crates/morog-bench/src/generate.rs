//! Synthetic STA vault generator.
//!
//! Transforms the downloaded corpus into a vault of topic / argument /
//! source notes (roughly 60/20/20 by file count) matching the frontmatter
//! shapes of plan.md section 2.2. `[@key]` cites and `#anchor` tags are
//! emitted even though morg-parser does not understand them yet — they
//! exercise the parser as plain text / unknown tags, which is what the
//! throughput benchmark needs.

use std::fs;
use std::io;
use std::path::Path;
use std::time::Instant;

use crate::corpus::Corpus;
use crate::rng::Rng;

const STANCES: [&str; 3] = ["supports", "opposes", "qualifies"];
const OPS: [&str; 6] = [
    "recall", "explain", "apply", "analyze", "evaluate", "create",
];
const SOURCE_STATUS: [&str; 4] = ["unread", "skimmed", "read", "processed"];
const ARG_STATUS: [&str; 3] = ["draft", "developing", "stable"];
const CREATED: &str = "2026-09-30";

#[derive(Default)]
pub struct VaultStats {
    pub topics: usize,
    pub arguments: usize,
    pub sources: usize,
    pub words: u64,
    pub bytes: u64,
}

impl VaultStats {
    pub fn files(&self) -> usize {
        self.topics + self.arguments + self.sources
    }
}

struct Gen<'a> {
    corpus: &'a Corpus,
    rng: Rng,
    cursor: usize,
    topic_ids: Vec<String>,
    source_ids: Vec<String>,
    argument_ids: Vec<String>,
}

pub fn run(corpus_dir: &Path, out: &Path, target_words: u64, seed: u64) -> io::Result<VaultStats> {
    let t0 = Instant::now();
    let corpus = Corpus::load(corpus_dir)?;
    println!(
        "corpus: {} words loaded from {} in {:.1}s",
        corpus.len(),
        corpus_dir.display(),
        t0.elapsed().as_secs_f64()
    );

    if out.exists() {
        fs::remove_dir_all(out)?;
    }
    fs::create_dir_all(out)?;

    let mut g = Gen::new(&corpus, seed, target_words);
    let stats = g.emit(out, target_words)?;

    println!(
        "vault: {} files ({} topics, {} arguments, {} sources), {} words, {:.1} MiB in {:.1}s",
        stats.files(),
        stats.topics,
        stats.arguments,
        stats.sources,
        stats.words,
        stats.bytes as f64 / (1024.0 * 1024.0),
        t0.elapsed().as_secs_f64()
    );
    Ok(stats)
}

impl<'a> Gen<'a> {
    fn new(corpus: &'a Corpus, seed: u64, target_words: u64) -> Self {
        let mut g = Self {
            corpus,
            rng: Rng::new(seed),
            cursor: 0,
            topic_ids: Vec::new(),
            source_ids: Vec::new(),
            argument_ids: Vec::new(),
        };
        // Plan the id space up front so notes can cross-link to notes that
        // are emitted later. Sized from the expected words-per-cycle; a few
        // unused ids at the tail just become dangling targets, which the
        // design explicitly tolerates.
        let cycles = (target_words / 1000).max(1) as usize;
        for i in 0..cycles * 3 {
            let id = format!("{}-{}-{i:05}", g.slug_word(), g.slug_word());
            g.topic_ids.push(id);
        }
        for i in 0..cycles {
            let year = 1800 + g.rng.below(226);
            let id = format!("{}_{}_{year}_{i:04}", g.slug_word(), g.slug_word());
            g.source_ids.push(id);
        }
        for i in 0..cycles {
            let id = format!("{}-{}-claim-{i:04}", g.slug_word(), g.slug_word());
            g.argument_ids.push(id);
        }
        g
    }

    /// Emit notes in a fixed T,T,S,T,A cycle (60/20/20) until the word
    /// budget is reached.
    fn emit(&mut self, out: &Path, target_words: u64) -> io::Result<VaultStats> {
        let mut stats = VaultStats::default();
        let mut cycle = 0usize;
        while stats.words < target_words {
            for step in 0..5u8 {
                let (kind_dir, id, content) = match step {
                    0 | 1 | 3 => {
                        let idx = cycle * 3 + usize::from(step.min(2));
                        let id = self.topic_ids[idx].clone();
                        stats.topics += 1;
                        ("topics", id.clone(), self.topic_note(&id))
                    }
                    2 => {
                        let id = self.source_ids[cycle].clone();
                        stats.sources += 1;
                        ("sources", id.clone(), self.source_note(&id))
                    }
                    _ => {
                        let id = self.argument_ids[cycle].clone();
                        stats.arguments += 1;
                        ("arguments", id.clone(), self.argument_note(&id))
                    }
                };
                let shard = format!("{:03}", cycle / 100);
                let dir = out.join(kind_dir).join(shard);
                fs::create_dir_all(&dir)?;
                let path = dir.join(format!("{id}.md"));
                stats.words += content.split_whitespace().count() as u64;
                stats.bytes += content.len() as u64;
                fs::write(path, content)?;
                if stats.words >= target_words {
                    break;
                }
            }
            cycle += 1;
        }
        Ok(stats)
    }

    // -- note builders ------------------------------------------------------

    fn topic_note(&mut self, id: &str) -> String {
        let title = self.title(2, 4);
        let uuid = self.uuid();
        let mut s = String::with_capacity(4096);
        s.push_str("---\n");
        s.push_str(&format!("id: {id}\n"));
        s.push_str(&format!("uuid: {uuid}\n"));
        s.push_str("kind: topic\n");
        s.push_str(&format!("title: \"{title}\"\n"));
        if self.rng.chance(30) {
            let a = self.slug_word().to_uppercase();
            let b = self.slug_word();
            s.push_str(&format!("aliases: [{a}, {b}]\n"));
        }
        if self.rng.chance(50) {
            let n = self.rng.range(1, 3);
            let mut related = Vec::with_capacity(n);
            for _ in 0..n {
                related.push(self.pick_topic().to_string());
            }
            s.push_str(&format!("related: [{}]\n", related.join(", ")));
        }
        s.push_str(&format!("created: {CREATED}\n"));
        s.push_str("---\n\n");
        s.push_str(&format!("# {title}\n\n"));

        let blocks = self.rng.range(4, 8);
        let mut anchor_n = 0usize;
        for _ in 0..blocks {
            if self.rng.chance(20) {
                self.push_list_block(&mut s);
            } else {
                self.push_topic_paragraph(&mut s, &mut anchor_n);
            }
            s.push('\n');
        }
        s
    }

    fn push_topic_paragraph(&mut self, s: &mut String, anchor_n: &mut usize) {
        let words = self.rng.range(40, 90);
        s.push_str(&self.prose(words, 35));
        // Provenance marker: cite, #mine, or (rarely) nothing.
        let roll = self.rng.below(100);
        if roll < 70 {
            s.push(' ');
            s.push_str(&self.cite());
            if self.rng.chance(20) {
                s.push(' ');
                s.push_str(&self.cite());
            }
        } else if roll < 95 {
            s.push_str(" #mine");
        }
        if self.rng.chance(30) {
            *anchor_n += 1;
            s.push_str(&format!(" #anchor evidence-{anchor_n}"));
        }
        s.push('\n');
    }

    fn push_list_block(&mut self, s: &mut String) {
        for _ in 0..self.rng.range(3, 5) {
            let words = self.rng.range(15, 30);
            s.push_str("- ");
            s.push_str(&self.prose(words, 15));
            s.push(' ');
            s.push_str(&self.cite());
            s.push('\n');
        }
    }

    fn argument_note(&mut self, id: &str) -> String {
        let title = self.title(3, 6);
        let uuid = self.uuid();
        let thesis = self.sentence(12, 20);
        let form = if self.rng.chance(85) {
            "claim"
        } else {
            "question"
        };
        let n_rel = self.rng.range(2, 6);
        let mut targets: Vec<String> = Vec::with_capacity(n_rel);
        let mut s = String::with_capacity(2048);
        s.push_str("---\n");
        s.push_str(&format!("id: {id}\n"));
        s.push_str(&format!("uuid: {uuid}\n"));
        s.push_str("kind: argument\n");
        s.push_str(&format!("title: \"{title}\"\n"));
        s.push_str(&format!("form: {form}\n"));
        s.push_str(&format!("thesis: \"{thesis}\"\n"));
        s.push_str("relations:\n");
        for _ in 0..n_rel {
            let topic = self.pick_topic().to_string();
            let to = if self.rng.chance(25) {
                format!("{topic}#evidence-{}", self.rng.range(1, 3))
            } else {
                topic.clone()
            };
            let stance = *self.rng.pick(&STANCES);
            if self.rng.chance(70) {
                let op = *self.rng.pick(&OPS);
                s.push_str(&format!("  - {{ to: {to}, stance: {stance}, op: {op} }}\n"));
            } else {
                s.push_str(&format!("  - {{ to: {to}, stance: {stance} }}\n"));
            }
            targets.push(topic);
        }
        s.push_str(&format!("status: {}\n", self.rng.pick(&ARG_STATUS)));
        s.push_str("publish: false\n");
        s.push_str(&format!("created: {CREATED}\n"));
        s.push_str("---\n\n");
        s.push_str(&format!("# {title}\n\n"));
        s.push_str(&thesis);
        s.push_str("\n\n");
        for _ in 0..self.rng.range(1, 3) {
            let words = self.rng.range(50, 90);
            let mut para = self.prose(words, 0);
            let target = self.rng.pick(&targets).clone();
            let label = self.title(1, 2);
            para.push_str(&format!(" See [{label}](id:{target})."));
            s.push_str(&para);
            s.push_str("\n\n");
        }
        s
    }

    fn source_note(&mut self, id: &str) -> String {
        let title = self.title(3, 7);
        let uuid = self.uuid();
        let mut s = String::with_capacity(512);
        s.push_str("---\n");
        s.push_str(&format!("id: {id}\n"));
        s.push_str(&format!("uuid: {uuid}\n"));
        s.push_str("kind: source\n");
        s.push_str(&format!("title: \"{title}\"\n"));
        s.push_str(&format!("status: {}\n", self.rng.pick(&SOURCE_STATUS)));
        s.push_str(&format!("created: {CREATED}\n"));
        s.push_str("---\n");
        if self.rng.chance(30) {
            s.push('\n');
            let words = self.rng.range(20, 40);
            s.push_str(&self.prose(words, 0));
            s.push('\n');
        }
        s
    }

    // -- text primitives ----------------------------------------------------

    fn next_word(&mut self) -> &str {
        let w = self.corpus.word(self.cursor);
        self.cursor += 1;
        w
    }

    /// A paragraph of `n` corpus words; with probability `link_pct`, one
    /// inline `[text](id:...)` link is spliced in.
    fn prose(&mut self, n: usize, link_pct: usize) -> String {
        let link_at = if link_pct > 0 && self.rng.chance(link_pct) {
            Some(self.rng.below(n))
        } else {
            None
        };
        let mut out = String::with_capacity(n * 7);
        for i in 0..n {
            if i > 0 {
                out.push(' ');
            }
            if link_at == Some(i) {
                let label = self.title(1, 3);
                let target = self.pick_any_id().to_string();
                out.push_str(&format!("[{label}](id:{target})"));
            } else {
                let w = self.next_word().to_string();
                out.push_str(&w);
            }
        }
        out
    }

    fn sentence(&mut self, lo: usize, hi: usize) -> String {
        let n = self.rng.range(lo, hi);
        let mut s = self.prose(n, 0);
        s.retain(|c| c != '"');
        let mut out: String = s
            .trim_end_matches(['.', ',', ';', '!', '?'])
            .trim()
            .to_string();
        out.push('.');
        out
    }

    fn title(&mut self, lo: usize, hi: usize) -> String {
        let n = self.rng.range(lo, hi);
        let words: Vec<String> = (0..n)
            .map(|_| {
                let w = self.slug_word();
                let mut c = w.chars();
                match c.next() {
                    Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                    None => w,
                }
            })
            .collect();
        words.join(" ")
    }

    /// A lowercase ascii-alphabetic word for ids, slugs, and anchors.
    fn slug_word(&mut self) -> String {
        loop {
            let w: String = self
                .next_word()
                .chars()
                .filter(char::is_ascii_alphabetic)
                .collect::<String>()
                .to_lowercase();
            if w.len() >= 3 {
                return w;
            }
        }
    }

    fn cite(&mut self) -> String {
        let key = self.rng.pick(&self.source_ids).clone();
        if self.rng.chance(25) {
            format!("[@{key}, p. {}]", self.rng.range(1, 400))
        } else {
            format!("[@{key}]")
        }
    }

    fn pick_topic(&mut self) -> &str {
        let i = self.rng.below(self.topic_ids.len());
        &self.topic_ids[i]
    }

    fn pick_any_id(&mut self) -> &str {
        match self.rng.below(10) {
            0..=5 => {
                let i = self.rng.below(self.topic_ids.len());
                &self.topic_ids[i]
            }
            6..=7 => {
                let i = self.rng.below(self.source_ids.len());
                &self.source_ids[i]
            }
            _ => {
                let i = self.rng.below(self.argument_ids.len());
                &self.argument_ids[i]
            }
        }
    }

    /// Deterministic v4-shaped uuid from the PRNG.
    fn uuid(&mut self) -> String {
        let a = self.rng.next_u64();
        let b = self.rng.next_u64();
        format!(
            "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
            a >> 32,
            (a >> 16) & 0xffff,
            a & 0xfff,
            0x8000 | (b >> 48) & 0x3fff,
            b & 0xffff_ffff_ffff
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid_is_shaped_like_a_uuid() {
        let corpus = test_corpus();
        let mut g = Gen::new(&corpus, 1, 1000);
        let u = g.uuid();
        assert_eq!(u.len(), 36);
        assert_eq!(u.matches('-').count(), 4);
        assert_eq!(u.as_bytes()[14], b'4');
    }

    #[test]
    fn notes_parse_cleanly_with_morg_parser() {
        let corpus = test_corpus();
        let mut g = Gen::new(&corpus, 99, 30_000);
        for i in 0..20 {
            let topic_id = g.topic_ids[i].clone();
            let arg_id = g.argument_ids[i].clone();
            let src_id = g.source_ids[i].clone();
            for content in [
                g.topic_note(&topic_id),
                g.argument_note(&arg_id),
                g.source_note(&src_id),
            ] {
                let result = morg_parser::parse_document(&content);
                assert!(
                    result.errors.is_empty(),
                    "parse errors in generated note:\n{content}\n{:?}",
                    result.errors
                );
                let fm = result.document.frontmatter.expect("frontmatter present");
                let id = fm.data.as_mapping_get("id").and_then(|v| v.as_str());
                assert!(id.is_some(), "frontmatter id parses as yaml");
            }
        }
    }

    #[test]
    fn generation_is_deterministic() {
        let corpus = test_corpus();
        let mut a = Gen::new(&corpus, 7, 2000);
        let mut b = Gen::new(&corpus, 7, 2000);
        let id_a = a.topic_ids[0].clone();
        let id_b = b.topic_ids[0].clone();
        assert_eq!(id_a, id_b);
        assert_eq!(a.topic_note(&id_a), b.topic_note(&id_b));
    }

    fn test_corpus() -> Corpus {
        let text = "the quick brown fox jumps over the lazy dog and runs far away
            into deep woods where tall trees grow beside quiet rivers full of
            silver fish that swim beneath cold water under bright autumn skies"
            .repeat(50);
        Corpus::from_text(&text)
    }
}
