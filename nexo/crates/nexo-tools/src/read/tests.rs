use super::*;
use crate::util::{resolve, MAX_FILE_BYTES};
#[cfg(unix)]
use crate::write::Write;
use base64::Engine;
use nexo_core::message::ImageSource;
use nexo_core::ToolDisplay;

fn ctx(dir: &std::path::Path) -> ToolContext {
    ToolContext::new(dir.to_path_buf())
}

#[tokio::test]
async fn a_notebook_reads_as_its_cells_without_the_base64_of_its_pictures() {
    let d = tempfile::tempdir().unwrap();
    let png = "A".repeat(40_000);
    let nb = json!({"cells": [
        {"cell_type": "markdown", "source": ["# Fit\n", "A line."]},
        {"cell_type": "code", "source": "print(1)", "outputs": [
            {"output_type": "stream", "text": ["1\n"]},
            {"output_type": "display_data", "data": {"image/png": png, "text/plain": ["<Figure>"]}},
            {"output_type": "error", "ename": "ValueError", "evalue": "bad", "traceback": ["\u{1b}[31mlong"]}
        ]}
    ], "metadata": {}, "nbformat": 4});
    std::fs::write(d.path().join("fit.ipynb"), nb.to_string()).unwrap();
    let out = Read.run(json!({"path": "fit.ipynb"}), &ctx(d.path())).await;
    assert!(!out.is_error, "{}", out.content);
    let c = &out.content;
    assert!(c.starts_with("(2 notebook cells shown as text;"), "{c}");
    for want in [
        "│ [cell 1 · markdown]",
        "│ # Fit",
        "│ A line.",
        "│ [cell 2 · code]",
        "│ print(1)",
        "│ 1",
        "│ <Figure>",
        "│ (image/png, 40 KiB, not shown)",
        "│ ValueError: bad",
    ] {
        assert!(c.contains(want), "no {want:?} in\n{c}");
    }
    assert!(!c.contains("AAAA") && !c.contains("traceback"), "{c}");
    // a file named .ipynb that is not a notebook reads as it is
    std::fs::write(d.path().join("x.ipynb"), "not json\n").unwrap();
    let out = Read.run(json!({"path": "x.ipynb"}), &ctx(d.path())).await;
    assert_eq!(out.content, "     1│ not json\n");
}

#[tokio::test]
async fn rust_symbol_lookup_returns_exact_span_and_reports_ambiguity() {
    let d = tempfile::tempdir().unwrap();
    let c = ctx(d.path());
    std::fs::write(
        d.path().join("a.rs"),
        "// before\n#[inline]\nfn target() {\n    let s = \"}\";\n}\nfn other() {}\n",
    )
    .unwrap();
    let out = Read.run(json!({"path":"a.rs", "symbol":"target"}), &c).await;
    assert!(!out.is_error, "{}", out.content);
    assert!(out.content.contains("fn target()") && out.content.contains("let s"), "{}", out.content);
    assert!(!out.content.contains("fn other"), "{}", out.content);
    assert!(matches!(out.display, ToolDisplay::File { start_line: 2, .. }));
    let missing = Read.run(json!({"path":"a.rs", "symbol":"missing"}), &c).await;
    assert!(missing.is_error && missing.content.contains("not found"));
    std::fs::write(d.path().join("a.rs"), "impl A { fn same(&self) {} }\nimpl B { fn same(&self) {} }\n").unwrap();
    let ambiguous = Read.run(json!({"path":"a.rs", "symbol":"same"}), &c).await;
    assert!(
        ambiguous.is_error && ambiguous.content.contains("ambiguous") && ambiguous.content.contains("1-1, 2-2"),
        "{}",
        ambiguous.content
    );
    for args in [
        json!({"path":"a.rs", "symbol":"same", "offset":1}),
        json!({"paths":["a.rs", "b.rs"], "symbol":"same"}),
        json!({"path":"a.rs", "symbol":""}),
    ] {
        assert!(Read.run(args, &c).await.is_error);
    }
    std::fs::write(d.path().join("a.py"), "def same(): pass\n").unwrap();
    assert!(Read.run(json!({"path":"a.py", "symbol":"same"}), &c).await.content.contains("Rust"));
    std::fs::write(d.path().join("a.rs"), "fn broken(").unwrap();
    assert!(Read.run(json!({"path":"a.rs", "symbol":"broken"}), &c).await.content.contains("cannot parse"));
}

#[tokio::test]
async fn long_symbol_is_capped_with_a_continuation() {
    let d = tempfile::tempdir().unwrap();
    let text = format!("fn long() {{\n{}\n}}\n", "    let x = 1;\n".repeat(6000));
    std::fs::write(d.path().join("a.rs"), text).unwrap();
    let out = Read.run(json!({"path":"a.rs", "symbol":"long"}), &ctx(d.path())).await;
    assert!(
        !out.is_error && out.content.contains("symbol truncated") && out.content.contains("offset"),
        "{}",
        out.content
    );
    assert!(out.content.chars().count() < FILE_CAP + 200);
}

#[tokio::test]
async fn reads_numbered_lines_with_a_window() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("a.txt"), "one\ntwo\nthree\nfour\n").unwrap();
    let out = Read.run(json!({"path": "a.txt"}), &ctx(d.path())).await;
    assert!(!out.is_error);
    assert!(out.content.starts_with("     1│ one\n     2│ two\n"));
    let out = Read.run(json!({"path": "a.txt", "offset": 2, "limit": 2}), &ctx(d.path())).await;
    assert_eq!(out.content, "     2│ two\n     3│ three\n… 1 more lines (of 4); read with offset 4 to continue\n");
    match out.display {
        ToolDisplay::File { start_line, text, .. } => {
            assert_eq!(start_line, 2);
            assert_eq!(text, "two\nthree\n");
        }
        _ => panic!(),
    }
    assert_eq!(Read.summary(&json!({"path": "a.txt", "offset": 2, "limit": 2})), "read a.txt [2..3]");
}

#[tokio::test]
async fn refuses_what_it_cannot_read() {
    let d = tempfile::tempdir().unwrap();
    let c = ctx(d.path());
    assert!(Read.run(json!({}), &c).await.is_error);
    let out = Read.run(json!({"path": "missing.txt"}), &c).await;
    assert!(out.is_error && out.content.contains("does not exist"));
    assert!(Read.run(json!({"path": "."}), &c).await.content.contains("directory"));
    std::fs::write(d.path().join("b.bin"), b"\x00\x01\x02").unwrap();
    assert!(Read.run(json!({"path": "b.bin"}), &c).await.content.contains("binary"));
    std::fs::write(d.path().join("e.txt"), "").unwrap();
    let out = Read.run(json!({"path": "e.txt"}), &c).await;
    assert!(!out.is_error && out.content.contains("empty file"));
    std::fs::write(d.path().join("l.txt"), "x".repeat(3000)).unwrap();
    let out = Read.run(json!({"path": "l.txt"}), &c).await;
    assert!(out.content.ends_with("…\n"));
    assert!(out.content.chars().count() < 2100);
    let out = Read.run(json!({"path": "l.txt", "offset": 9}), &c).await;
    assert!(out.is_error && out.content.contains("past the end"));
    let big = std::fs::File::create(d.path().join("big.log")).unwrap();
    big.set_len(MAX_FILE_BYTES + 1).unwrap(); // sparse: no bytes are written
    let out = Read.run(json!({"path": "big.log"}), &c).await;
    assert!(out.is_error && out.content.contains("too large"), "{}", out.content);
}

#[cfg(unix)]
#[tokio::test]
async fn a_device_is_refused_not_read() {
    let d = tempfile::tempdir().unwrap();
    let out = Read.run(json!({"path": "/dev/zero"}), &ctx(d.path())).await;
    assert!(out.is_error && out.content.contains("not a regular file"), "{}", out.content);
    let out = Write.run(json!({"path": "/dev/zero", "content": "x"}), &ToolContext::new(d.path().to_path_buf())).await;
    assert!(out.is_error && out.content.contains("not a regular file"), "{}", out.content);
}

#[test]
fn a_zero_offset_and_limit_do_not_underflow_the_summary() {
    let s = Read.summary(&json!({"path": "a", "offset": 0, "limit": 0}));
    assert_eq!(s, "read a [0..0]");
}

fn png_file(d: &std::path::Path, name: &str, w: u32, h: u32) {
    std::fs::write(d.join(name), crate::picture::tests::png(w, h)).unwrap();
}

#[tokio::test]
async fn a_png_comes_back_as_an_image_with_a_size_line() {
    let d = tempfile::tempdir().unwrap();
    png_file(d.path(), "n.png", 30, 12);
    let out = Read.run(json!({"path": "n.png"}), &ctx(d.path())).await;
    assert!(!out.is_error, "{}", out.content);
    assert!(out.content.starts_with("n.png: PNG image of 30x12 px, "), "{}", out.content);
    assert!(out.content.ends_with(" bytes"));
    match out.images.as_slice() {
        [ImageSource::Base64 { media_type, data }] => {
            assert_eq!(media_type, "image/png");
            let back = base64::engine::general_purpose::STANDARD.decode(data).unwrap();
            assert_eq!(back, std::fs::read(d.path().join("n.png")).unwrap());
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn the_magic_bytes_decide_not_the_extension() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("shot.dat"), crate::picture::tests::png(4, 4)).unwrap();
    assert_eq!(Read.run(json!({"path": "shot.dat"}), &ctx(d.path())).await.images.len(), 1);
    std::fs::write(d.path().join("fake.png"), "just text\n").unwrap();
    let out = Read.run(json!({"path": "fake.png"}), &ctx(d.path())).await;
    assert!(out.images.is_empty() && out.content.contains("just text"), "{}", out.content);
}

#[tokio::test]
async fn a_model_without_vision_gets_text_only() {
    let d = tempfile::tempdir().unwrap();
    png_file(d.path(), "n.png", 30, 12);
    let mut c = ctx(d.path());
    c.vision = false;
    let out = Read.run(json!({"path": "n.png"}), &c).await;
    assert!(!out.is_error && out.images.is_empty());
    assert!(out.content.contains("30x12") && out.content.contains("cannot view images"), "{}", out.content);
}

#[tokio::test]
async fn a_picture_over_the_cap_is_refused_with_the_size() {
    let d = tempfile::tempdir().unwrap();
    png_file(d.path(), "wide.png", 2001, 2);
    let out = Read.run(json!({"path": "wide.png"}), &ctx(d.path())).await;
    assert!(out.is_error && out.images.is_empty(), "{}", out.content);
    assert!(out.content.contains("2001x2") && out.content.contains("too large"), "{}", out.content);
    // within 2000 px but over 5 MB: the header is real, the body is padding
    let mut big = crate::picture::tests::png(8, 8);
    big.resize(crate::picture::MAX_BYTES + 1, 0);
    std::fs::write(d.path().join("heavy.png"), big).unwrap();
    let out = Read.run(json!({"path": "heavy.png"}), &ctx(d.path())).await;
    assert!(out.is_error && out.content.contains("5 MB"), "{}", out.content);
}

#[tokio::test]
async fn a_ppm_and_a_bmp_come_back_as_png_images() {
    let d = tempfile::tempdir().unwrap();
    let mut ppm = b"P6\n2 1\n255\n".to_vec();
    ppm.extend([10, 20, 30, 40, 50, 60]);
    std::fs::write(d.path().join("s.ppm"), &ppm).unwrap();
    let out = Read.run(json!({"path": "s.ppm"}), &ctx(d.path())).await;
    assert!(!out.is_error, "{}", out.content);
    assert_eq!(out.content, format!("s.ppm: PPM image of 2x1 px, {} bytes, shown as PNG", ppm.len()));
    match out.images.as_slice() {
        [ImageSource::Base64 { media_type, data }] => {
            assert_eq!(media_type, "image/png");
            let png = base64::engine::general_purpose::STANDARD.decode(data).unwrap();
            let img = image::load_from_memory_with_format(&png, image::ImageFormat::Png).unwrap().to_rgb8();
            assert_eq!(img.into_raw(), vec![10, 20, 30, 40, 50, 60]);
        }
        other => panic!("{other:?}"),
    }
    let mut c = ctx(d.path());
    c.vision = false;
    let out = Read.run(json!({"path": "s.ppm"}), &c).await;
    assert!(!out.is_error && out.images.is_empty() && out.content.contains("2x1"), "{}", out.content);
    assert!(out.content.contains("cannot view images"), "{}", out.content);
}

#[tokio::test]
async fn a_raster_over_the_cap_is_refused() {
    let d = tempfile::tempdir().unwrap();
    let mut f = b"P5\n2001 2\n255\n".to_vec();
    f.resize(f.len() + 2001 * 2, 7);
    std::fs::write(d.path().join("wide.pgm"), f).unwrap();
    let out = Read.run(json!({"path": "wide.pgm"}), &ctx(d.path())).await;
    assert!(out.is_error && out.images.is_empty(), "{}", out.content);
    assert!(out.content.contains("2001x2") && out.content.contains("too large"), "{}", out.content);
    std::fs::write(d.path().join("cut.ppm"), b"P6 9 9 255\nabc").unwrap();
    let out = Read.run(json!({"path": "cut.ppm"}), &ctx(d.path())).await;
    assert!(out.is_error && out.content.contains("cut short"), "{}", out.content);
}

#[tokio::test]
async fn a_tiff_gets_a_convert_hint_not_binary_file() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("scan.tiff"), b"II*\0\x08\0\0\0\0\0").unwrap();
    let out = Read.run(json!({"path": "scan.tiff"}), &ctx(d.path())).await;
    assert!(out.is_error && out.images.is_empty());
    assert!(out.content.contains("TIFF image") && out.content.contains("magick in.tiff out.png"), "{}", out.content);
    assert!(!out.content.contains("binary file"));
}

#[tokio::test]
async fn several_files_come_back_under_headers_and_one_failure_does_not_fail_the_rest() {
    let d = tempfile::tempdir().unwrap();
    let c = ctx(d.path());
    std::fs::write(d.path().join("a.txt"), "one\ntwo\n").unwrap();
    std::fs::write(d.path().join("b.txt"), "three\nfour\nfive\n").unwrap();
    std::fs::write(d.path().join("z.bin"), b"\x00\x01").unwrap();
    std::fs::create_dir(d.path().join("sub")).unwrap();
    let out = Read.run(json!({"paths": ["a.txt", "missing.txt", "b.txt", "z.bin", "sub"]}), &c).await;
    assert!(!out.is_error, "{}", out.content);
    let t = &out.content;
    assert!(t.contains("=== a.txt ===\n     1│ one\n     2│ two\n"), "{t}");
    assert!(t.contains("=== missing.txt (failed) ===\nread: missing.txt does not exist"), "{t}");
    assert!(t.contains("=== b.txt ===\n     1│ three\n"), "{t}");
    assert!(t.contains("=== z.bin (failed) ===") && t.contains("binary"), "{t}");
    assert!(t.contains("=== sub (failed) ===") && t.contains("directory"), "{t}");
    // the order asked for is the order given
    assert!(t.find("a.txt").unwrap() < t.find("b.txt").unwrap());
    match &out.display {
        ToolDisplay::List { items } => {
            assert_eq!(items[0], "a.txt: 2 lines");
            assert_eq!(items[1], "missing.txt: failed");
            assert_eq!(items.len(), 5);
        }
        _ => panic!("{:?}", out.display),
    }
    // both files that were read count as seen
    assert_eq!(c.seen.unseen(&resolve(d.path(), "a.txt")), None);
    assert_eq!(c.seen.unseen(&resolve(d.path(), "b.txt")), None);
    // all failing is an error
    let out = Read.run(json!({"paths": ["nope1", "nope2"]}), &c).await;
    assert!(out.is_error && out.content.contains("=== nope2 (failed) ==="), "{}", out.content);
}

#[tokio::test]
async fn path_and_paths_are_one_list_and_a_single_file_reads_as_before() {
    let d = tempfile::tempdir().unwrap();
    let c = ctx(d.path());
    std::fs::write(d.path().join("a.txt"), "one\ntwo\nthree\n").unwrap();
    std::fs::write(d.path().join("b.txt"), "x\ny\nz\n").unwrap();
    let plain = Read.run(json!({"path": "a.txt"}), &c).await;
    let one = Read.run(json!({"paths": ["a.txt", "a.txt", " "]}), &c).await;
    assert_eq!(one.content, plain.content);
    let both = Read.run(json!({"path": "a.txt", "paths": ["b.txt", "a.txt"]}), &c).await;
    assert_eq!(both.content.matches("=== a.txt ===").count(), 1, "{}", both.content);
    assert!(both.content.contains("=== b.txt ==="));
    // the window applies to each file
    let w = Read.run(json!({"paths": ["a.txt", "b.txt"], "offset": 2, "limit": 1}), &c).await;
    assert!(w.content.contains("=== a.txt ===\n     2│ two\n… 1 more lines"), "{}", w.content);
    assert!(w.content.contains("=== b.txt ===\n     2│ y\n"), "{}", w.content);
    let none = Read.run(json!({"paths": []}), &c).await;
    assert!(none.is_error && none.content.contains("`path` or `paths`"), "{}", none.content);
    assert_eq!(Read.summary(&json!({"paths": ["a.txt", "b.txt"]})), "read a.txt, b.txt");
    assert_eq!(Read.summary(&json!({"paths": ["a", "b", "c", "d"]})), "read a, b (+2)");
}

#[tokio::test]
async fn a_call_is_capped_as_a_whole_and_says_how_to_go_on() {
    let d = tempfile::tempdir().unwrap();
    let c = ctx(d.path());
    let big: String =
        (0..3000).map(|i| format!("line number {i} with some padding to make it long enough\n")).collect();
    for n in ["a.txt", "b.txt", "c.txt"] {
        std::fs::write(d.path().join(n), &big).unwrap();
    }
    let out = Read.run(json!({"paths": ["a.txt", "b.txt", "c.txt"]}), &c).await;
    assert!(!out.is_error);
    assert!(out.content.len() < MULTI_CAP + 2_000, "{}", out.content.len());
    assert!(out.content.contains("the call hit its size cap, so read with offset"), "capped note missing");
    assert!(out.content.contains("=== c.txt (not read) ==="), "last file should be left out");
    let many: Vec<String> = (0..=MAX_PATHS).map(|i| format!("f{i}")).collect();
    let out = Read.run(json!({"paths": many}), &c).await;
    assert!(out.is_error && out.content.contains("at most"), "{}", out.content);
}

/// Rebuilds a file from the successive reads that the trailers lead, and checks the cap each time.
async fn read_all(c: &ToolContext, name: &str) -> (String, usize) {
    let (mut got, mut offset, mut calls) = (String::new(), 1usize, 0usize);
    loop {
        let out = Read.run(json!({"path": name, "offset": offset}), c).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.chars().count() <= FILE_CAP + 200, "over the cap: {}", out.content.len());
        calls += 1;
        match out.content.rsplit_once("… lines ") {
            Some((body, trailer)) => {
                got.push_str(body);
                let next: usize =
                    trailer.rsplit("read with offset ").next().unwrap().split(' ').next().unwrap().parse().unwrap();
                assert!(next > offset, "no progress: {trailer}");
                offset = next;
            }
            None => {
                got.push_str(&out.content);
                return (got, calls);
            }
        }
    }
}

#[tokio::test]
async fn a_file_under_the_cap_comes_back_unchanged_and_over_it_is_cut_at_a_line() {
    let d = tempfile::tempdir().unwrap();
    let c = ctx(d.path());
    std::fs::write(d.path().join("s.txt"), "one\ntwo\n").unwrap();
    let out = Read.run(json!({"path": "s.txt"}), &c).await;
    assert_eq!(out.content, "     1│ one\n     2│ two\n");
    let text: String = (1..=401).map(|i| format!("{i} {}\n", "x".repeat(380))).collect();
    std::fs::write(d.path().join("b.txt"), &text).unwrap();
    let out = Read.run(json!({"path": "b.txt"}), &c).await;
    assert!(out.content.chars().count() <= FILE_CAP + 200, "{}", out.content.len());
    let (body, trailer) = out.content.rsplit_once("… lines ").unwrap();
    assert!(body.ends_with('\n') && trailer.starts_with("1-"), "{trailer}");
    assert!(trailer.contains("of 401 returned") && trailer.contains("read with offset"), "{trailer}");
    // reading on with the offsets the trailers give: no gap, no overlap, to the end
    let (all, calls) = read_all(&c, "b.txt").await;
    assert!(calls >= 2);
    let want: String = text.lines().enumerate().map(|(i, l)| format!("{:>6}│ {l}\n", i + 1)).collect();
    assert_eq!(all, want);
    // an explicit window is capped the same way
    let out = Read.run(json!({"path": "b.txt", "offset": 10, "limit": 300}), &c).await;
    assert!(out.content.contains("… lines 10-") && out.content.contains("of 401 returned"), "{}", out.content);
}

#[tokio::test]
async fn a_giant_first_line_and_multibyte_text_at_the_cut_do_not_panic() {
    let d = tempfile::tempdir().unwrap();
    let c = ctx(d.path());
    std::fs::write(d.path().join("g.txt"), format!("{}\nnext\n", "é".repeat(200_000))).unwrap();
    let out = Read.run(json!({"path": "g.txt"}), &c).await;
    assert!(!out.is_error && out.content.chars().count() < FILE_CAP, "{}", out.content.len());
    let text: String = (1..=300).map(|i| format!("{i} {}\n", "日".repeat(1990))).collect();
    std::fs::write(d.path().join("m.txt"), &text).unwrap();
    let (all, calls) = read_all(&c, "m.txt").await;
    assert!(calls >= 2 && all.contains("   300│ 300 日"), "{calls}");
    let out = Read.run(json!({"paths": ["m.txt", "g.txt"]}), &c).await;
    assert!(out.content.contains("=== g.txt"), "{}", out.content.len());
}
