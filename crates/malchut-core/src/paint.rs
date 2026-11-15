//! [`DisplayList`] を Cairo に描く。

use std::f64::consts::{FRAC_PI_2, PI};

use crate::layout::{DisplayList, Item};
use crate::picture::Picture;
use crate::theme::Color;

/// 論理座標のまま描く。拡大率は呼び出し側が `cr.scale` で決める。
pub fn paint(cr: &cairo::Context, list: &DisplayList) -> Result<(), cairo::Error> {
    for item in &list.items {
        match item {
            Item::Rect {
                x,
                y,
                width,
                height,
                radius,
                color,
            } => {
                set_color(cr, *color);
                rounded_rect(cr, *x, *y, *width, *height, *radius);
                cr.fill()?;
            }
            Item::Text {
                x,
                y,
                layout,
                color,
            } => {
                set_color(cr, *color);
                cr.move_to(*x, *y);
                pangocairo::functions::show_layout(cr, layout);
            }
            Item::Svg {
                x,
                y,
                width,
                height,
                image,
            } => image.paint(cr, *x, *y, *width, *height),
            Item::Image {
                x,
                y,
                width,
                height,
                picture,
                source,
                radius,
                shadow,
                opacity,
                tint,
            } => {
                if *shadow {
                    paint_shadow(cr, *x, *y, *width, *height, *radius)?;
                }
                cr.save()?;
                rounded_rect(cr, *x, *y, *width, *height, *radius);
                cr.clip();
                cr.translate(*x, *y);
                let painted = match tint {
                    None => picture.paint(cr, *source, *width, *height, *opacity),
                    Some(tint) => paint_tinted(cr, picture, *source, *width, *height, *opacity, *tint),
                };
                cr.restore()?;
                painted?;
            }
        }
    }
    cr.status()
}

/// 指定の拡大率で画像に描く (サムネイルやテスト用)。
pub fn render_image(list: &DisplayList, scale: f64) -> Result<cairo::ImageSurface, cairo::Error> {
    let surface = cairo::ImageSurface::create(
        cairo::Format::Rgb24,
        (list.width * scale).round() as i32,
        (list.height * scale).round() as i32,
    )?;
    let cr = cairo::Context::new(&surface)?;
    cr.scale(scale, scale);
    paint(&cr, list)?;
    drop(cr);
    Ok(surface)
}

/// 画像に色を重ねて描く。呼び出し側で、画像の矩形にクリップしてあること。
fn paint_tinted(
    cr: &cairo::Context,
    picture: &Picture,
    source: (f64, f64, f64, f64),
    width: f64,
    height: f64,
    opacity: f64,
    tint: Color,
) -> Result<(), cairo::Error> {
    // 透明部分のない画像なら、上から塗るだけで済む。PDF でも元の画像データのまま残る。
    if picture.is_opaque() && opacity >= 1.0 {
        cr.save()?;
        let painted = picture.paint(cr, source, width, height, 1.0);
        cr.restore()?;
        painted?;
        set_color(cr, tint);
        return cr.paint();
    }
    // 透明部分には色を載せず、全体の不透明度は色を重ねた結果に掛ける。
    cr.push_group();
    cr.save()?;
    let painted = picture.paint(cr, source, width, height, 1.0);
    cr.restore()?;
    painted?;
    cr.set_operator(cairo::Operator::Atop);
    set_color(cr, tint);
    cr.paint()?;
    cr.pop_group_to_source()?;
    cr.paint_with_alpha(opacity)
}

/// 少しずつ広げた半透明の矩形を重ねて、ぼかした影に見せる。
fn paint_shadow(
    cr: &cairo::Context,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    radius: f64,
) -> Result<(), cairo::Error> {
    const LAYERS: u32 = 12;
    const SPREAD: f64 = 2.0;
    const OFFSET_Y: f64 = 10.0;
    cr.set_source_rgba(0.0, 0.0, 0.0, 0.02);
    for layer in 1..=LAYERS {
        let grow = f64::from(layer) * SPREAD;
        rounded_rect(
            cr,
            x - grow,
            y - grow + OFFSET_Y,
            width + 2.0 * grow,
            height + 2.0 * grow,
            radius + grow,
        );
        cr.fill()?;
    }
    Ok(())
}

fn set_color(cr: &cairo::Context, color: Color) {
    cr.set_source_rgba(color.r, color.g, color.b, color.a);
}

fn rounded_rect(cr: &cairo::Context, x: f64, y: f64, width: f64, height: f64, radius: f64) {
    let radius = radius.min(width / 2.0).min(height / 2.0);
    if radius <= 0.0 {
        cr.rectangle(x, y, width, height);
        return;
    }
    cr.new_sub_path();
    cr.arc(x + width - radius, y + radius, radius, -FRAC_PI_2, 0.0);
    cr.arc(x + width - radius, y + height - radius, radius, 0.0, FRAC_PI_2);
    cr.arc(x + radius, y + height - radius, radius, FRAC_PI_2, PI);
    cr.arc(x + radius, y + radius, radius, PI, 3.0 * FRAC_PI_2);
    cr.close_path();
}
