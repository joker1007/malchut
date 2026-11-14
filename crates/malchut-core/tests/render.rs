use malchut_core::export::layout_deck;
use malchut_core::paint::render_image;
use malchut_core::parse::parse;
use malchut_core::theme::Theme;

#[test]
fn every_builtin_theme_renders() {
    let deck = parse("# Title\n\ntext `code`\n\n---\n<!-- class: invert lead -->\n\n```rust\nfn f() {}\n```\n");
    for name in malchut_core::theme::BUILTIN_THEMES {
        let theme = Theme::builtin(name).unwrap();
        for list in layout_deck(&deck, &theme, std::path::Path::new(".")) {
            assert!(!list.overflow);
            let image = render_image(&list, 0.25).unwrap();
            assert_eq!((image.width(), image.height()), (480, 270));
        }
    }
}

#[test]
fn overflow_is_detected() {
    let deck = parse(&"paragraph\n\n".repeat(40));
    let theme = Theme::builtin("default").unwrap();
    assert!(layout_deck(&deck, &theme, std::path::Path::new("."))[0].overflow);
}

#[test]
fn images_render_and_missing_ones_do_not_fail() {
    let examples = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    let deck = parse(
        "![w:300](assets/photo.jpg) ![w:300](assets/logo.svg) ![w:300](assets/missing.png)\n\n\
         ![bg left:30%](assets/square.png)\n![at:50%,50% anchor:center](assets/logo.svg)\n",
    );
    let theme = Theme::builtin("default").unwrap();
    let lists = layout_deck(&deck, &theme, &examples);
    assert_eq!(lists[0].assets.len(), 5);
    assert!(!lists[0].overflow);
    render_image(&lists[0], 0.25).unwrap();
}

#[test]
fn link_areas_follow_the_text() {
    let deck = parse("text [link](#1) more\n\n| a |\n|---|\n| [cell](https://example.com) |\n");
    let theme = Theme::builtin("default").unwrap();
    let list = &layout_deck(&deck, &theme, std::path::Path::new("."))[0];
    assert_eq!(list.links.len(), 2);
    let link = &list.links[0];
    assert_eq!(link.url, "#1");
    // 段落の先頭 ("text ") より右から始まり、スライドの余白の内側にある。
    assert!(link.x > 110.0 && link.width > 0.0 && link.y >= 80.0);
    let inside = (link.x + link.width / 2.0, link.y + link.height / 2.0);
    assert_eq!(list.link_at(inside.0, inside.1), Some("#1"));
    assert_eq!(list.link_at(link.x - 20.0, inside.1), None);
    assert_eq!(list.links[1].url, "https://example.com");
}
