//! Integration tests: `morog publish` over the committed publish fixture
//! vault (pubvault) — site generation, template overrides, JSON-LD islands,
//! unlinked private targets, and deterministic output.

use std::path::{Path, PathBuf};
use std::process::Output;

fn pubvault() -> &'static str {
    concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/pubvault")
}

fn fixture_bibrat() -> &'static str {
    concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/bibrat")
}

const BASE: &str = "https://notes.example.com";
/// The fixture uuids (sources/topics/arguments of pubvault).
const WRIGHT_UUID: &str = "0192f3c7-7a11-7bbb-8f55-aaaaaaaaaaa1";
const TOPIC_UUID: &str = "0192f3c7-7a11-7bbb-8f55-aaaaaaaaaaa3";
const PRIVATE_UUID: &str = "0192f3c7-7a11-7bbb-8f55-aaaaaaaaaaa4";
const ARG_UUID: &str = "0192f3c7-7a11-7bbb-8f55-aaaaaaaaaaa5";

/// Temp tree acting as XDG_CACHE_HOME and XDG_CONFIG_HOME for one test, so
/// runs touch neither the real cache nor the real user config.
struct TempHome(PathBuf);

impl TempHome {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "morog-publish-test-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("config/morog")).unwrap();
        Self(dir)
    }

    /// A home with `[publish] base_iri` configured and pubvault indexed
    /// against the fixture bibrat store (wright_leaf_2004 carries a DOI).
    fn ready(tag: &str) -> Self {
        let home = Self::new(tag);
        home.write_config(&format!("[publish]\nbase_iri = \"{BASE}\"\n"));
        home.ok(&["index", "--bib-root", fixture_bibrat()]);
        home
    }

    fn write_config(&self, text: &str) {
        std::fs::write(self.0.join("config/morog/config.toml"), text).unwrap();
    }

    fn out_dir(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    fn run(&self, args: &[&str]) -> Output {
        std::process::Command::new(env!("CARGO_BIN_EXE_morog"))
            .args(args)
            .current_dir(pubvault())
            .env("XDG_CACHE_HOME", self.0.join("cache"))
            .env("XDG_CONFIG_HOME", self.0.join("config"))
            .output()
            .expect("run morog")
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "`morog {}` failed:\nstdout: {}\nstderr: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        String::from_utf8(out.stdout).unwrap()
    }

    fn site(&self, name: &str) -> PathBuf {
        let dir = self.out_dir(name);
        self.ok(&["publish", "site", "-o", dir.to_str().unwrap()]);
        dir
    }
}

impl Drop for TempHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The JSON-LD island of a rendered page, parsed.
fn jsonld_of(page: &str) -> serde_json::Value {
    let start = page
        .find("<script type=\"application/ld+json\">")
        .expect("JSON-LD island")
        + "<script type=\"application/ld+json\">".len();
    let end = page[start..].find("</script>").expect("island closes") + start;
    serde_json::from_str(page[start..end].trim()).expect("JSON-LD parses as JSON")
}

#[test]
fn site_requires_base_iri() {
    let home = TempHome::new("no-base");
    home.ok(&["index", "--no-bib"]);
    let out = home.run(&[
        "publish",
        "site",
        "-o",
        home.out_dir("site").to_str().unwrap(),
    ]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("base_iri"), "{stderr}");
}

#[test]
fn site_requires_an_existing_index() {
    let home = TempHome::new("no-index");
    home.write_config(&format!("[publish]\nbase_iri = \"{BASE}\"\n"));
    let out = home.run(&[
        "publish",
        "site",
        "-o",
        home.out_dir("site").to_str().unwrap(),
    ]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("run `morog index` first"), "{stderr}");
}

#[test]
fn site_publishes_only_published_notes() {
    let home = TempHome::ready("pages");
    let site = home.site("site");

    // One page per published note under /n/<uuid>, an alias page per
    // readable id, the kind-grouped index, and the stylesheet.
    for rel in [
        "index.html",
        "style.css",
        &format!("n/{WRIGHT_UUID}/index.html"),
        &format!("n/{TOPIC_UUID}/index.html"),
        &format!("n/{ARG_UUID}/index.html"),
        "wright_leaf_2004/index.html",
        "leaf-economics/index.html",
        "allocation-argument/index.html",
    ] {
        assert!(site.join(rel).is_file(), "missing {rel}");
    }
    // Private notes get no page anywhere.
    assert!(!site.join(format!("n/{PRIVATE_UUID}")).exists());
    assert!(!site.join("shade-plasticity").exists());
    assert!(!site.join("reich_global_1997").exists());

    // The index groups published notes by kind and never mentions private
    // ones.
    let index = read(&site.join("index.html"));
    for needle in [
        "<h2>Arguments</h2>",
        "<h2>Topics</h2>",
        "<h2>Sources</h2>",
        "allocation-argument",
        "leaf-economics",
        "wright_leaf_2004",
    ] {
        assert!(index.contains(needle), "missing {needle} in:\n{index}");
    }
    assert!(!index.contains("shade-plasticity"), "{index}");
    assert!(!index.contains("reich_global_1997"), "{index}");

    // Alias pages redirect the readable id to the canonical uuid IRI.
    let alias = read(&site.join("leaf-economics/index.html"));
    assert!(
        alias.contains(&format!(
            "<meta http-equiv=\"refresh\" content=\"0; url={BASE}/n/{TOPIC_UUID}\">"
        )),
        "{alias}"
    );
}

#[test]
fn note_page_links_published_and_unlinks_private() {
    let home = TempHome::ready("content");
    let site = home.site("site");
    let topic = read(&site.join(format!("n/{TOPIC_UUID}/index.html")));

    // Evidence blocks carry their anchors as element ids; the citation of a
    // published source links to its note page, locator preserved.
    assert!(
        topic.contains(&format!(
            "<p class=\"block evidence\" id=\"spectrum\">SLA anchors the worldwide leaf \
             economics spectrum <span class=\"cite\">[<a href=\"{BASE}/n/{WRIGHT_UUID}\">\
             wright_leaf_2004</a>, p. 822]</span>.</p>"
        )),
        "{topic}"
    );
    // A citation of a private source note renders unlinked (pub::leak warns).
    assert!(
        topic.contains("<span class=\"cite\">[reich_global_1997]</span>"),
        "{topic}"
    );
    // A bib-only citekey (virtual source, no DOI in the store) stays plain.
    assert!(
        topic.contains("<span class=\"cite\">[poorter_biomass_2012]</span>"),
        "{topic}"
    );
    // The prose link to the private topic is unlinked text.
    assert!(
        topic.contains("I suspect shade plasticity confounds"),
        "{topic}"
    );
    assert!(!topic.contains("shade-plasticity\""), "{topic}");
    assert!(!topic.contains(PRIVATE_UUID), "{topic}");
    // The consumed tags are stripped from the rendered text.
    assert!(!topic.contains("#anchor"), "{topic}");
    assert!(!topic.contains("#mine"), "{topic}");

    // Backlinks: the published argument's relations point here.
    assert!(topic.contains("<section class=\"backlinks\">"), "{topic}");
    assert!(topic.contains("(supports, analyze)"), "{topic}");

    // The argument page: anchored relation links, private target unlinked.
    let arg = read(&site.join(format!("n/{ARG_UUID}/index.html")));
    assert!(
        arg.contains(&format!("<a href=\"{BASE}/n/{TOPIC_UUID}#spectrum\">")),
        "{arg}"
    );
    assert!(
        arg.contains("<span class=\"stance\">qualifies</span> <span class=\"op\">(evaluate)</span> → Shade plasticity</li>"),
        "{arg}"
    );
}

#[test]
fn note_page_embeds_parseable_jsonld() {
    let home = TempHome::ready("jsonld");
    let site = home.site("site");

    let topic = jsonld_of(&read(&site.join(format!("n/{TOPIC_UUID}/index.html"))));
    assert_eq!(topic["@id"], format!("{BASE}/n/{TOPIC_UUID}"));
    assert_eq!(topic["@type"][1], "morog:Topic");
    assert_eq!(topic["identifier"], "leaf-economics");
    assert_eq!(topic["alternateName"][0], "LES");
    // CiTO citation only for the citekey whose bib entry carries a DOI.
    assert_eq!(
        topic["cito:cites"][0]["@id"],
        "https://doi.org/10.1038/nature02403"
    );
    assert_eq!(
        topic["cito:cites"][0]["name"],
        "The worldwide leaf economics spectrum"
    );
    assert_eq!(topic["cito:cites"].as_array().unwrap().len(), 1);
    // The private `related:` target never appears.
    assert!(!topic.to_string().contains("shade"), "{topic}");

    let arg = jsonld_of(&read(&site.join(format!("n/{ARG_UUID}/index.html"))));
    assert_eq!(arg["@type"][0], "Claim");
    assert_eq!(
        arg["text"],
        "Leaf economics variation is biomass allocation in disguise."
    );
    let argues = arg["morog:argues"].as_array().unwrap();
    // Three published targets; the qualifies-relation to the private topic
    // is excluded.
    assert_eq!(argues.len(), 3);
    assert_eq!(argues[0]["morog:stance"], "supports");
    assert_eq!(argues[0]["morog:op"], "analyze");
    assert_eq!(
        argues[0]["morog:target"]["@id"],
        format!("{BASE}/n/{TOPIC_UUID}#spectrum")
    );
    let context = arg["@context"][1].as_object().unwrap();
    assert_eq!(context["morog"], "https://morog.example/ns#");
    assert_eq!(context["cito"], "http://purl.org/spar/cito/");
}

#[test]
fn site_dir_overrides_template_snippets_and_sees_extra() {
    let home = TempHome::ready("override");
    let theme = home.0.join("theme");
    std::fs::create_dir_all(&theme).unwrap();
    std::fs::write(
        theme.join("note.html"),
        "{% extends \"base.html\" %}\n{% block content %}CUSTOM-THEME layout={{ note.extra.layout }} id={{ note.id }}{% endblock %}\n",
    )
    .unwrap();
    home.write_config(&format!(
        "[publish]\nbase_iri = \"{BASE}\"\nsite_dir = \"{}\"\n",
        theme.display()
    ));

    let site = home.site("site");
    let topic = read(&site.join(format!("n/{TOPIC_UUID}/index.html")));
    // The overridden snippet renders, with `extra:` frontmatter exposed,
    // inside the untouched embedded base template.
    assert!(
        topic.contains("CUSTOM-THEME layout=wide id=leaf-economics"),
        "{topic}"
    );
    assert!(
        topic.contains("Generated by <code>morog publish site</code>"),
        "{topic}"
    );
    // Other pages still use the default snippets.
    let index = read(&site.join("index.html"));
    assert!(index.contains("<h2>Topics</h2>"), "{index}");
}

#[test]
fn site_output_is_deterministic() {
    let home = TempHome::ready("determinism");
    let first = home.site("site-a");
    let second = home.site("site-b");

    let mut files = Vec::new();
    collect(&first, &mut files);
    files.sort();
    assert!(!files.is_empty());
    for rel in files {
        assert_eq!(
            read(&first.join(&rel)),
            read(&second.join(&rel)),
            "{rel} differs between runs"
        );
    }
}

fn collect(root: &Path, out: &mut Vec<String>) {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, root, out);
            } else {
                out.push(path.strip_prefix(root).unwrap().display().to_string());
            }
        }
    }
    walk(root, root, out);
}

// ---------------------------------------------------------------- rdf dump

/// Parse emitted Turtle with a real parser; panics on any syntax error.
fn parse_turtle(text: &str) -> Vec<String> {
    oxttl::TurtleParser::new()
        .for_slice(text.as_bytes())
        .map(|t| {
            let t = t.expect("emitted Turtle parses");
            format!("{} {} {}", t.subject, t.predicate, t.object)
        })
        .collect()
}

#[test]
fn rdf_dump_parses_and_covers_the_published_subgraph() {
    let home = TempHome::ready("rdf");
    let turtle = home.ok(&["publish", "rdf"]);
    let triples = parse_turtle(&turtle);
    assert!(triples.len() >= 20, "suspiciously small dump:\n{turtle}");

    let has = |needle: &str| {
        assert!(
            triples.iter().any(|t| t.contains(needle)),
            "no triple containing `{needle}` in:\n{}",
            triples.join("\n")
        );
    };
    // Notes are typed resources under the base IRI.
    has(&format!(
        "<{BASE}/n/{ARG_UUID}> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> \
         <https://morog.example/ns#Argument>"
    ));
    has(&format!(
        "<{BASE}/n/{TOPIC_UUID}> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> \
         <https://morog.example/ns#Topic>"
    ));
    // Typed relations carry morog:stance and morog:op.
    has("<https://morog.example/ns#stance> <https://morog.example/ns#supports>");
    has("<https://morog.example/ns#stance> <https://morog.example/ns#opposes>");
    has("<https://morog.example/ns#op> <https://morog.example/ns#analyze>");
    has(&format!(
        "<https://morog.example/ns#target> <{BASE}/n/{TOPIC_UUID}#spectrum>"
    ));
    // PROV-O derivation from the evidence block to its published source.
    has(&format!(
        "<{BASE}/n/{TOPIC_UUID}#spectrum> <http://www.w3.org/ns/prov#wasDerivedFrom> \
         <{BASE}/n/{WRIGHT_UUID}>"
    ));
    // CiTO citation where the bibliography has a DOI.
    has(&format!(
        "<{BASE}/n/{TOPIC_UUID}#spectrum> <http://purl.org/spar/cito/cites> \
         <https://doi.org/10.1038/nature02403>"
    ));

    // The private topic never enters the dump: no qualifies relation, no
    // IRI, no text.
    assert!(!turtle.contains(PRIVATE_UUID), "{turtle}");
    assert!(!turtle.contains("shade"), "{turtle}");
    assert!(!turtle.contains("qualifies"), "{turtle}");
}

#[test]
fn rdf_output_file_and_determinism() {
    let home = TempHome::ready("rdf-det");
    let stdout = home.ok(&["publish", "rdf"]);

    let file = home.out_dir("dump.ttl");
    let silent = home.ok(&["publish", "rdf", "-o", file.to_str().unwrap()]);
    assert!(silent.is_empty(), "{silent}");
    assert_eq!(read(&file), stdout, "file and stdout dumps differ");

    // Two runs emit byte-identical Turtle.
    assert_eq!(home.ok(&["publish", "rdf"]), stdout);
}

// ------------------------------------------------------------- micropub

/// Find the @graph node with the given @id.
fn node<'a>(graph: &'a [serde_json::Value], id: &str) -> &'a serde_json::Value {
    graph
        .iter()
        .find(|n| n["@id"] == id)
        .unwrap_or_else(|| panic!("no node `{id}` in {graph:?}"))
}

#[test]
fn micropub_bundles_one_argument_support_graph_each() {
    let home = TempHome::ready("micropub");
    let dir = home.out_dir("mp");
    home.ok(&["publish", "micropub", "-o", dir.to_str().unwrap()]);

    // One bundle per published argument, named by its readable id.
    let mut files: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    files.sort();
    assert_eq!(files, ["allocation-argument.jsonld"]);

    let text = read(&dir.join("allocation-argument.jsonld"));
    let bundle: serde_json::Value = serde_json::from_str(&text).expect("bundle parses as JSON");
    let context = bundle["@context"].as_object().unwrap();
    assert_eq!(context["mp"], "http://purl.org/mp/");
    let graph = bundle["@graph"].as_array().unwrap();

    // The micropublication argues its thesis as the principal claim.
    let arg_iri = format!("{BASE}/n/{ARG_UUID}");
    let claim_id = format!("{arg_iri}#claim");
    let root = node(graph, &arg_iri);
    assert_eq!(root["@type"], "mp:Micropublication");
    assert_eq!(root["mp:argues"]["@id"], claim_id);
    let claim = node(graph, &claim_id);
    assert_eq!(claim["@type"], "mp:Claim");
    assert_eq!(
        claim["text"],
        "Leaf economics variation is biomass allocation in disguise."
    );

    // Support graph: evidence blocks as statements, stance → mp predicate,
    // Bloom op preserved.
    let spectrum = node(graph, &format!("{BASE}/n/{TOPIC_UUID}#spectrum"));
    assert_eq!(spectrum["@type"], "mp:Statement");
    assert_eq!(spectrum["mp:supports"]["@id"], claim_id);
    assert_eq!(spectrum["morog:op"], "analyze");
    assert!(
        spectrum["text"]
            .as_str()
            .unwrap()
            .starts_with("SLA anchors"),
        "{spectrum}"
    );
    let photo = node(graph, &format!("{BASE}/n/{TOPIC_UUID}#photosynthesis"));
    assert_eq!(photo["mp:challenges"]["@id"], claim_id);

    // Attributions: statements derive from reference nodes carrying the
    // bibliography metadata. The published source note is the reference id
    // where one exists; a bib-only key becomes a bundle-local node.
    let wright_iri = format!("{BASE}/n/{WRIGHT_UUID}");
    assert_eq!(spectrum["prov:wasDerivedFrom"][0]["@id"], wright_iri);
    let wright = node(graph, &wright_iri);
    assert_eq!(wright["name"], "The worldwide leaf economics spectrum");
    assert_eq!(
        wright["dcterms:identifier"],
        "https://doi.org/10.1038/nature02403"
    );
    let poorter = node(graph, &format!("{arg_iri}#ref-poorter_biomass_2012"));
    assert_eq!(poorter["morog:citekey"], "poorter_biomass_2012");

    // The qualifies relation targets a private topic: it stays out, and the
    // private note leaks into nothing.
    assert!(!text.contains("shade"), "{text}");
    assert!(!text.contains(PRIVATE_UUID), "{text}");
    assert!(!text.contains("qualifies"), "{text}");

    // Deterministic output.
    home.ok(&[
        "publish",
        "micropub",
        "-o",
        home.out_dir("mp2").to_str().unwrap(),
    ]);
    assert_eq!(
        text,
        read(&home.out_dir("mp2").join("allocation-argument.jsonld"))
    );
}
