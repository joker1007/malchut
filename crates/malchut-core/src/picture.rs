//! 画像ファイル (ラスタ画像と SVG) の読み込みと描画。

use std::path::Path;

use crate::diagram::SvgImage;

pub enum Picture {
    Raster(cairo::ImageSurface),
    Svg(SvgImage),
}

impl std::fmt::Debug for Picture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (width, height) = self.size();
        write!(f, "Picture({width}x{height})")
    }
}

impl Picture {
    pub fn load(path: &Path) -> Result<Picture, String> {
        let is_svg = path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("svg"));
        if is_svg {
            return SvgImage::from_file(path).map(Picture::Svg);
        }
        let pixbuf = gdk_pixbuf::Pixbuf::from_file(path).map_err(|err| err.to_string())?;
        let rotated = pixbuf.option("orientation").is_some_and(|o| o != "1");
        // カメラ画像の向き (EXIF) を反映する。
        let pixbuf = pixbuf.apply_embedded_orientation().unwrap_or(pixbuf);
        let surface = surface_from_pixbuf(&pixbuf).map_err(|err| err.to_string())?;

        // JPEG は元データを添付し、PDF に再圧縮なしで埋め込ませる。
        let is_jpeg = path.extension().is_some_and(|ext| {
            ext.eq_ignore_ascii_case("jpg") || ext.eq_ignore_ascii_case("jpeg")
        });
        if is_jpeg
            && !rotated
            && let Ok(bytes) = std::fs::read(path)
        {
            let _ = surface.set_mime_data(cairo::MIME_TYPE_JPEG, bytes);
        }
        Ok(Picture::Raster(surface))
    }

    /// 画像本来の大きさ (px)。
    pub fn size(&self) -> (f64, f64) {
        match self {
            Picture::Raster(surface) => (f64::from(surface.width()), f64::from(surface.height())),
            Picture::Svg(svg) => (svg.width, svg.height),
        }
    }

    /// 透明な部分を持たない画像かどうか。
    pub fn is_opaque(&self) -> bool {
        matches!(self, Picture::Raster(surface) if surface.format() == cairo::Format::Rgb24)
    }

    /// 元画像の `source` (x, y, 幅, 高さ) の範囲を、原点から幅 `width`・高さ `height` に描く。
    /// 範囲外にはみ出す部分は、呼び出し側のクリップで切り落とす。
    pub fn paint(
        &self,
        cr: &cairo::Context,
        source: (f64, f64, f64, f64),
        width: f64,
        height: f64,
        opacity: f64,
    ) -> Result<(), cairo::Error> {
        let (source_x, source_y, source_width, source_height) = source;
        cr.scale(width / source_width, height / source_height);
        cr.translate(-source_x, -source_y);
        match self {
            Picture::Raster(surface) => {
                cr.set_source_surface(surface, 0.0, 0.0)?;
                cr.source().set_filter(cairo::Filter::Good);
                cr.paint_with_alpha(opacity)
            }
            Picture::Svg(svg) if opacity >= 1.0 => {
                svg.paint(cr, 0.0, 0.0, svg.width, svg.height);
                Ok(())
            }
            Picture::Svg(svg) => {
                cr.push_group();
                svg.paint(cr, 0.0, 0.0, svg.width, svg.height);
                cr.pop_group_to_source()?;
                cr.paint_with_alpha(opacity)
            }
        }
    }
}

/// Pixbuf (RGB / RGBA、非乗算) を Cairo の画像に変換する。アルファを持たない画像は RGB24、
/// 持つ画像は ARGB32 (乗算済み) にする。
fn surface_from_pixbuf(pixbuf: &gdk_pixbuf::Pixbuf) -> Result<cairo::ImageSurface, cairo::Error> {
    let (width, height) = (pixbuf.width(), pixbuf.height());
    let channels = pixbuf.n_channels() as usize;
    let source_stride = pixbuf.rowstride() as usize;
    let pixels = pixbuf.read_pixel_bytes();
    let format = if channels == 4 {
        cairo::Format::ARgb32
    } else {
        cairo::Format::Rgb24
    };
    let mut surface = cairo::ImageSurface::create(format, width, height)?;
    let stride = surface.stride() as usize;
    {
        let mut data = surface.data().map_err(|_| cairo::Error::SurfaceFinished)?;
        for row in 0..height as usize {
            for column in 0..width as usize {
                let source = &pixels[row * source_stride + column * channels..];
                let alpha = if channels == 4 { u32::from(source[3]) } else { 255 };
                let premultiply = |value: u8| (u32::from(value) * alpha + 127) / 255;
                let pixel = (alpha << 24)
                    | (premultiply(source[0]) << 16)
                    | (premultiply(source[1]) << 8)
                    | premultiply(source[2]);
                let offset = row * stride + column * 4;
                data[offset..offset + 4].copy_from_slice(&pixel.to_ne_bytes());
            }
        }
    }
    Ok(surface)
}
