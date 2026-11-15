use {
    super::{
        geom::{Point, Rect, Size, Transform},
        image::{SubimageMut, R},
        num::Zero,
    },
    makepad_rustybuzz as rustybuzz,
    rustybuzz::ttf_parser,
};

#[derive(Clone, Debug)]
pub struct GlyphOutline {
    bounds: Rect<f32>,
    units_per_em: f32,
    commands: Vec<Command>,
}

impl GlyphOutline {
    pub fn origin_in_ems(&self) -> Point<f32> {
        self.bounds.origin / self.units_per_em
    }

    pub fn size_in_ems(&self) -> Size<f32> {
        self.bounds.size / self.units_per_em
    }

    pub fn bounds_in_ems(&self) -> Rect<f32> {
        Rect::new(self.origin_in_ems(), self.size_in_ems())
    }

    pub fn rasterize(&self, dpxs_per_em: f32, output: &mut SubimageMut<R>) {
        use ab_glyph_rasterizer::Rasterizer;

        fn to_ab_glyph(p: Point<f32>) -> ab_glyph_rasterizer::Point {
            ab_glyph_rasterizer::point(p.x, p.y)
        }

        let output_size = output.bounds().size;
        let mut rasterizer = Rasterizer::new(output_size.width, output_size.height);
        let origin = self.bounds.origin;
        let transform = Transform::from_translate(-origin.x, -origin.y)
            .scale_uniform(dpxs_per_em / self.units_per_em);
        let mut last = Point::ZERO;
        let mut last_move = None;
        for command in self.commands.iter().copied() {
            match command {
                Command::MoveTo(p) => {
                    last = p;
                    last_move = Some(p);
                }
                Command::LineTo(p) => {
                    rasterizer.draw_line(
                        to_ab_glyph(last.apply_transform(transform)),
                        to_ab_glyph(p.apply_transform(transform)),
                    );
                    last = p;
                }
                Command::QuadTo(p1, p) => {
                    rasterizer.draw_quad(
                        to_ab_glyph(last.apply_transform(transform)),
                        to_ab_glyph(p1.apply_transform(transform)),
                        to_ab_glyph(p.apply_transform(transform)),
                    );
                    last = p;
                }
                Command::CurveTo(p1, p2, p) => {
                    rasterizer.draw_cubic(
                        to_ab_glyph(last.apply_transform(transform)),
                        to_ab_glyph(p1.apply_transform(transform)),
                        to_ab_glyph(p2.apply_transform(transform)),
                        to_ab_glyph(p.apply_transform(transform)),
                    );
                    last = p;
                }
                Command::Close => {
                    if let Some(last_move) = last_move.take() {
                        rasterizer.draw_line(
                            to_ab_glyph(last.apply_transform(transform)),
                            to_ab_glyph(last_move.apply_transform(transform)),
                        );
                        last = last_move;
                    }
                }
            }
        }
        rasterizer.for_each_pixel_2d(|x, y, a| {
            let point = Point::new(x as usize, output_size.height - 1 - y as usize);
            let pixel = R::new((a * 255.0) as u8);
            output[point] = pixel;
        });
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Command {
    MoveTo(Point<f32>),
    LineTo(Point<f32>),
    QuadTo(Point<f32>, Point<f32>),
    CurveTo(Point<f32>, Point<f32>, Point<f32>),
    Close,
}

#[derive(Debug)]
pub struct Builder {
    commands: Vec<Command>,
}

impl Builder {
    pub fn new() -> Self {
        Self {
            commands: Vec::new(),
        }
    }

    pub fn finish(mut self, bounds: Rect<f32>, units_per_em: f32) -> GlyphOutline {
        self.close_contour();
        // ttf-parser returns an i16 box, truncating variable-font coordinates
        // toward zero. Negative fractional extrema can therefore lie outside
        // it. A clipped edge leaves the coverage rasterizer's winding nonzero
        // and fills the rest of the glyph with a rectangle. Include the emitted
        // points (and curve control hulls) before choosing the raster origin.
        let mut min = bounds.origin;
        let mut max = bounds.origin + bounds.size;
        let mut include = |p: Point<f32>| {
            min.x = min.x.min(p.x);
            min.y = min.y.min(p.y);
            max.x = max.x.max(p.x);
            max.y = max.y.max(p.y);
        };
        for command in &self.commands {
            match *command {
                Command::MoveTo(p) | Command::LineTo(p) => include(p),
                Command::QuadTo(p1, p) => {
                    include(p1);
                    include(p);
                }
                Command::CurveTo(p1, p2, p) => {
                    include(p1);
                    include(p2);
                    include(p);
                }
                Command::Close => {}
            }
        }
        GlyphOutline {
            bounds: Rect::new(min, max - min),
            units_per_em,
            commands: self.commands,
        }
    }

    fn close_contour(&mut self) {
        // Font contours are implicitly closed at the next move or end of the
        // outline. In particular, CFF2 has no endchar operator and the parser
        // can omit the final close callback. An open edge otherwise leaves
        // the coverage rasterizer's winding accumulator nonzero across rows.
        if !matches!(self.commands.last(), None | Some(Command::Close)) {
            self.commands.push(Command::Close);
        }
    }
}

impl ttf_parser::OutlineBuilder for Builder {
    fn move_to(&mut self, x: f32, y: f32) {
        self.close_contour();
        self.commands.push(Command::MoveTo(Point::new(x, y)));
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.commands.push(Command::LineTo(Point::new(x, y)));
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.commands
            .push(Command::QuadTo(Point::new(x1, y1), Point::new(x, y)));
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.commands.push(Command::CurveTo(
            Point::new(x1, y1),
            Point::new(x2, y2),
            Point::new(x, y),
        ));
    }

    fn close(&mut self) {
        self.close_contour();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::image::Image;
    use super::ttf_parser::OutlineBuilder;

    fn ring(close_before_move: bool, close_at_end: bool) -> GlyphOutline {
        let mut builder = Builder::new();
        builder.move_to(10.0, 10.0);
        builder.line_to(90.0, 10.0);
        builder.line_to(90.0, 90.0);
        builder.line_to(10.0, 90.0);
        if close_before_move {
            builder.close();
            builder.close();
        }
        // Opposite winding leaves a hole. Both contours end on a vertical
        // closing edge, as in the affected Korean CFF2 glyphs.
        builder.move_to(30.0, 30.0);
        builder.line_to(30.0, 70.0);
        builder.line_to(70.0, 70.0);
        builder.line_to(70.0, 30.0);
        if close_at_end {
            builder.close();
            builder.close();
        }
        builder.finish(Rect::new(Point::ZERO, Size::new(100.0, 100.0)), 100.0)
    }

    fn coverage(outline: &GlyphOutline) -> Image<R> {
        let size = Size::new(100, 100);
        let mut image = Image::new(size);
        outline.rasterize(100.0, &mut image.subimage_mut(size.into()));
        image
    }

    #[test]
    fn implicit_contour_closure_preserves_holes_and_exterior() {
        let expected = coverage(&ring(true, true));
        for (close_before_move, close_at_end) in [(false, true), (true, false), (false, false)] {
            let outline = ring(close_before_move, close_at_end);
            let actual = coverage(&outline);
            assert_eq!(actual.as_pixels(), expected.as_pixels());
            assert_eq!(actual[Point::new(20, 50)].r(), 255, "stroke");
            assert_eq!(actual[Point::new(50, 50)].r(), 0, "counter");
            assert_eq!(actual[Point::new(5, 50)].r(), 0, "left exterior");
            assert_eq!(actual[Point::new(95, 50)].r(), 0, "right exterior");
            assert_eq!(actual[Point::new(50, 5)].r(), 0, "top exterior");
            assert_eq!(actual[Point::new(50, 95)].r(), 0, "bottom exterior");
        }
    }

    #[test]
    fn fractional_slash_keeps_coverage_outside_the_stroke_clear() {
        fn slash(offset: f32, reverse: bool) -> GlyphOutline {
            let mut builder = Builder::new();
            let mut points = [(0.0, 0.0), (10.0, 0.0), (40.0, 100.0), (30.0, 100.0)];
            if reverse {
                points.reverse();
            }
            builder.move_to(points[0].0 + offset, points[0].1 + offset);
            for &(x, y) in &points[1..] {
                builder.line_to(x + offset, y + offset);
            }
            builder.close();
            // Match ttf-parser's truncation of the varied outline to i16.
            let min = Point::new(offset as i16 as f32, offset as i16 as f32);
            let max = Point::new((40.0 + offset) as i16 as f32, (100.0 + offset) as i16 as f32);
            builder.finish(Rect::new(min, max - min), 100.0)
        }

        for dpxs_per_em in [64.0, 128.0] {
            let size = Size::new((40.0_f32 * dpxs_per_em / 100.0).ceil() as usize, dpxs_per_em as usize);
            let mut expected = Image::new(size);
            slash(0.0, false).rasterize(dpxs_per_em, &mut expected.subimage_mut(size.into()));
            for reverse in [false, true] {
                let outline = slash(-0.5, reverse);
                let mut actual = Image::new(size);
                outline.rasterize(dpxs_per_em, &mut actual.subimage_mut(size.into()));
                for (actual, expected) in actual.as_pixels().iter().zip(expected.as_pixels()) {
                    assert!(actual.r().abs_diff(expected.r()) <= 1, "fractional slash at {dpxs_per_em} dpx/em");
                }
                assert_eq!(actual[Point::new(1, size.height / 2)].r(), 0, "left exterior");
                assert_eq!(actual[Point::new(size.width - 2, size.height / 2)].r(), 0, "right exterior");
                assert_eq!(actual[Point::new(size.width / 2, size.height / 2)].r(), 255, "diagonal stroke");
            }
        }
    }

    #[test]
    fn url_slash_raster_preserves_exterior_in_regular_and_semibold_faces() {
        let regular = include_bytes!("../../../makepad-widgets/resources/IBMPlexSans-Text.ttf");
        let semibold = include_bytes!("../../../makepad-widgets/resources/IBMPlexSans-SemiBold.ttf");
        let system = std::env::var_os("MAKEPAD_TEST_VARIABLE_FONT")
            .map(|path| std::fs::read(path).expect("explicit variable font fixture is readable"));
        let mut faces: Vec<(&[u8], f32)> = vec![(regular, 400.0), (semibold, 600.0)];
        if let Some(data) = &system {
            faces.extend([(data.as_slice(), 400.0), (data.as_slice(), 600.0)]);
        }
        for (data, weight) in faces {
            let mut face = ttf_parser::Face::parse(data, 0).expect("font fixture parses");
            if face.is_variable() {
                face.set_variation(ttf_parser::Tag::from_bytes(b"wght"), weight).expect("wght axis");
            }
            let glyph = face.glyph_index('/').expect("URL slash");
            let mut builder = Builder::new();
            let bounds = face.outline_glyph(glyph, &mut builder).expect("slash outline");
            let min = Point::new(bounds.x_min as f32, bounds.y_min as f32);
            let max = Point::new(bounds.x_max as f32, bounds.y_max as f32);
            let outline = builder.finish(Rect::new(min, max - min), face.units_per_em() as f32);
            for dpxs_per_em in [64.0, 128.0] {
                let extent = outline.size_in_ems() * dpxs_per_em;
                let size = Size::new(extent.width.ceil() as usize, extent.height.ceil() as usize);
                let mut image = Image::new(size);
                outline.rasterize(dpxs_per_em, &mut image.subimage_mut(size.into()));
                assert_eq!(image[Point::new(size.width / 8, size.height / 8)].r(), 0, "slash top-left exterior, weight {weight}");
                assert_eq!(image[Point::new(size.width * 7 / 8, size.height * 7 / 8)].r(), 0, "slash bottom-right exterior, weight {weight}");
                assert!(image[Point::new(size.width / 2, size.height / 2)].r() > 240, "slash stroke, weight {weight}");
            }
        }
    }
}
