use malchut_core::model::{Block, Severity};
use malchut_core::parse::parse;

const SOURCE: &str = r#"---
theme: dark
size: 4:3
paginate: true
---
<!-- _class: lead -->

# Title

<!-- 発表者ノート -->

---
<!-- footer: foot -->

- item
  - [x] nested

```rust {1,3-4} numbered
fn main() {}
```

```mermaid
graph LR
```

| a | b |
|:-:|--:|
| 1 | 2 |

---

> quote **bold** `code`

![w:50% round:8 shadow alt text](a.png "caption") ![h:300 crop:10%,0,80%,100%](b.svg)

![bg contain right:30% opacity:0.5](bg.jpg)
![side:left w:35%](side.png)
![at:90%,10% anchor:top-right w:120](logo.svg)
"#;

#[test]
fn deck_snapshot() {
    insta::assert_debug_snapshot!(parse(SOURCE));
}

#[test]
fn directives_scope() {
    let deck = parse(SOURCE);
    assert_eq!(deck.slides.len(), 3);
    assert_eq!(deck.slides[0].props.class, ["lead"]);
    assert!(deck.slides[1].props.class.is_empty());
    assert_eq!(deck.slides[0].props.footer, None);
    assert_eq!(deck.slides[2].props.footer.as_deref(), Some("foot"));
    assert_eq!(deck.slides[0].notes, ["発表者ノート"]);
    assert!(deck.diagnostics.is_empty());
}

#[test]
fn unknown_keys_are_reported() {
    let deck = parse("---\nthem: x\n---\n\n```rust wat\n```\n\n<!-- class: a\nfoo: b -->\n");
    let messages: Vec<&str> = deck.diagnostics.iter().map(|d| d.message.as_str()).collect();
    assert_eq!(messages.len(), 3, "{messages:?}");
    assert!(deck.diagnostics.iter().all(|d| d.severity == Severity::Warning));
}

#[test]
fn images_are_routed_by_placement() {
    let deck = parse(SOURCE);
    let slide = &deck.slides[2];
    assert_eq!(slide.backgrounds.len(), 1);
    assert_eq!(slide.floats.len(), 1);
    assert_eq!(slide.side.as_ref().map(|image| image.src.as_str()), Some("side.png"));
    let Some(Block::Images(images)) = slide.blocks.last() else {
        panic!("画像だけの段落が Images になっていない: {:?}", slide.blocks);
    };
    assert_eq!(images.len(), 2);
    assert_eq!(images[0].alt, "alt text");
    assert_eq!(images[0].caption.as_deref(), Some("caption"));
}

#[test]
fn bad_image_options_are_reported() {
    let deck = parse("![w:abc opacity:2 wat:1 left:40%](a.png)\n\ntext ![](b.png)\n");
    assert_eq!(deck.diagnostics.len(), 5, "{:?}", deck.diagnostics);
}

#[test]
fn links_resolve_to_slides_or_stay_external() {
    use malchut_core::model::LinkTarget;
    let deck = parse("# 最初\n\n[a](#2) [b](#次の-話) [c](https://example.com) [d](#99) [e](#nowhere)\n\n---\n\n## 次の 話\n");
    assert_eq!(deck.link_target("#2"), Some(LinkTarget::Slide(1)));
    assert_eq!(deck.link_target("#次の-話"), Some(LinkTarget::Slide(1)));
    assert_eq!(deck.link_target("#最初"), Some(LinkTarget::Slide(0)));
    assert_eq!(
        deck.link_target("https://example.com"),
        Some(LinkTarget::External("https://example.com"))
    );
    assert_eq!(deck.link_target("#99"), None);
    // 行き先の無いスライド内参照は、警告として報告する。
    assert_eq!(deck.diagnostics.len(), 2, "{:?}", deck.diagnostics);
}

#[test]
fn plain_comment_is_a_note() {
    let deck = parse("text\n\n<!-- TODO: あとで直す -->\n");
    assert_eq!(deck.slides[0].notes, ["TODO: あとで直す"]);
    assert!(matches!(deck.slides[0].blocks[..], [Block::Paragraph(_)]));
}

#[test]
fn broken_front_matter_is_an_error() {
    let deck = parse("---\ntheme: [\n---\n\n# x\n");
    assert!(deck.diagnostics.iter().any(|d| d.severity == Severity::Error));
}
