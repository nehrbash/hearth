use crate::scene::Bounds;
use anyhow::{Result, ensure};
use lyon_path::{Path, iterator::PathIterator, math::point};
use lyon_tessellation::{
    FillGeometryBuilder, FillOptions, FillTessellator, FillVertex, GeometryBuilder,
    GeometryBuilderError, StrokeGeometryBuilder, StrokeOptions, StrokeTessellator, StrokeVertex,
    VertexId,
};
pub use lyon_tessellation::{FillRule, LineCap, LineJoin};
use std::sync::Arc;

const MAX_VERTICES: usize = 65536;
const MAX_INDICES: usize = 393216;
const LIMIT: f32 = 1_000_000.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Command {
    Move([f32; 2]),
    Line([f32; 2]),
    Quad([f32; 2], [f32; 2]),
    Cubic([f32; 2], [f32; 2], [f32; 2]),
    Close,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fill {
    pub color: [f32; 4],
    pub rule: FillRule,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stroke {
    pub color: [f32; 4],
    pub width: f32,
    pub cap: LineCap,
    pub join: LineJoin,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Vertex {
    pub position: [f32; 2],
    pub color: [f32; 4],
}
#[derive(Debug, PartialEq)]
struct Mesh {
    vertices: Vec<Vertex>,
    indices: Vec<u32>,
    bounds: Bounds,
}
#[derive(Clone, Debug, PartialEq)]
pub struct PathData(Arc<Mesh>);

impl PathData {
    pub fn new(commands: &[Command], fill: Option<Fill>, stroke: Option<Stroke>) -> Result<Self> {
        ensure!(
            !commands.is_empty() && commands.len() <= 4096,
            "Path needs 1..=4096 commands"
        );
        ensure!(
            fill.is_some() || stroke.is_some(),
            "Path needs fill or stroke"
        );
        for color in fill
            .iter()
            .map(|f| f.color)
            .chain(stroke.iter().map(|s| s.color))
        {
            ensure!(
                color
                    .iter()
                    .all(|v| v.is_finite() && (0.0..=1.0).contains(v)),
                "Invalid path color"
            );
        }
        if let Some(stroke) = stroke {
            ensure!(
                stroke.width.is_finite() && stroke.width > 0.0 && stroke.width <= 4096.0,
                "Invalid stroke width"
            );
        }
        let mut builder = Path::builder();
        let mut open = false;
        for command in commands {
            let points: &[[f32; 2]] = match command {
                Command::Move(p) | Command::Line(p) => std::slice::from_ref(p),
                _ => &[],
            };
            for p in points {
                validate_point(*p)?;
            }
            match *command {
                Command::Move(p) => {
                    if open {
                        builder.end(false);
                    }
                    builder.begin(point(p[0], p[1]));
                    open = true;
                }
                Command::Line(p) => {
                    ensure!(open, "Line requires Move");
                    builder.line_to(point(p[0], p[1]));
                }
                Command::Quad(a, b) => {
                    ensure!(open, "Quad requires Move");
                    validate_point(a)?;
                    validate_point(b)?;
                    builder.quadratic_bezier_to(point(a[0], a[1]), point(b[0], b[1]));
                }
                Command::Cubic(a, b, c) => {
                    ensure!(open, "Cubic requires Move");
                    validate_point(a)?;
                    validate_point(b)?;
                    validate_point(c)?;
                    builder.cubic_bezier_to(
                        point(a[0], a[1]),
                        point(b[0], b[1]),
                        point(c[0], c[1]),
                    );
                }
                Command::Close => {
                    ensure!(open, "Close requires Move");
                    builder.end(true);
                    open = false;
                }
            }
        }
        if open {
            builder.end(false);
        }
        let path = builder.build();
        ensure!(
            path.iter().flattened(0.25).take(MAX_VERTICES + 1).count() <= MAX_VERTICES,
            "Path exceeds flattened segment budget"
        );
        let mut output = Output {
            vertices: Vec::new(),
            indices: Vec::new(),
            color: [0.0; 4],
            overflow: false,
        };
        if let Some(fill) = fill {
            output.color = fill.color;
            FillTessellator::new()
                .tessellate_path(
                    &path,
                    &FillOptions::default()
                        .with_tolerance(0.25)
                        .with_fill_rule(fill.rule),
                    &mut output,
                )
                .map_err(|e| anyhow::anyhow!("Path fill failed: {e:?}"))?;
        }
        if let Some(stroke) = stroke {
            output.color = stroke.color;
            let options = StrokeOptions::default()
                .with_tolerance(0.25)
                .with_line_width(stroke.width)
                .with_line_cap(stroke.cap)
                .with_line_join(stroke.join);
            StrokeTessellator::new()
                .tessellate_path(&path, &options, &mut output)
                .map_err(|e| anyhow::anyhow!("Path stroke failed: {e:?}"))?;
        }
        ensure!(!output.overflow, "Path exceeds mesh budget");
        let mut min = [f32::INFINITY; 2];
        let mut max = [f32::NEG_INFINITY; 2];
        for vertex in &output.vertices {
            validate_point(vertex.position)?;
            for axis in 0..2 {
                min[axis] = min[axis].min(vertex.position[axis]);
                max[axis] = max[axis].max(vertex.position[axis]);
            }
        }
        let bounds = if output.vertices.is_empty() {
            Bounds::new(0.0, 0.0, 0.0, 0.0)
        } else {
            Bounds::new(min[0], min[1], max[0] - min[0], max[1] - min[1])
        };
        Ok(Self(Arc::new(Mesh {
            vertices: output.vertices,
            indices: output.indices,
            bounds,
        })))
    }
    pub fn bounds(&self) -> Bounds {
        self.0.bounds
    }
    pub(crate) fn vertices(&self) -> &[Vertex] {
        &self.0.vertices
    }
    pub(crate) fn indices(&self) -> &[u32] {
        &self.0.indices
    }
}
fn validate_point(p: [f32; 2]) -> Result<()> {
    ensure!(
        p.iter().all(|v| v.is_finite() && v.abs() <= LIMIT),
        "Path coordinates exceed finite +/-1e6 range"
    );
    Ok(())
}
struct Output {
    vertices: Vec<Vertex>,
    indices: Vec<u32>,
    color: [f32; 4],
    overflow: bool,
}
impl Output {
    fn vertex(&mut self, p: [f32; 2]) -> std::result::Result<VertexId, GeometryBuilderError> {
        if self.vertices.len() >= MAX_VERTICES || self.overflow {
            return Err(GeometryBuilderError::TooManyVertices);
        }
        let id = VertexId(self.vertices.len() as u32);
        self.vertices.push(Vertex {
            position: p,
            color: self.color,
        });
        Ok(id)
    }
}
impl GeometryBuilder for Output {
    fn add_triangle(&mut self, a: VertexId, b: VertexId, c: VertexId) {
        if self.indices.len() + 3 > MAX_INDICES {
            self.overflow = true;
            return;
        }
        self.indices.extend([a.0, b.0, c.0]);
    }
}
impl FillGeometryBuilder for Output {
    fn add_fill_vertex(
        &mut self,
        vertex: FillVertex<'_>,
    ) -> std::result::Result<VertexId, GeometryBuilderError> {
        self.vertex(vertex.position().to_array())
    }
}
impl StrokeGeometryBuilder for Output {
    fn add_stroke_vertex(
        &mut self,
        vertex: StrokeVertex<'_, '_>,
    ) -> std::result::Result<VertexId, GeometryBuilderError> {
        self.vertex(vertex.position().to_array())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fill() -> Option<Fill> {
        Some(Fill {
            color: [1.0; 4],
            rule: FillRule::EvenOdd,
        })
    }
    #[test]
    fn malformed_and_unbounded_paths_are_rejected() {
        for commands in [
            vec![],
            vec![Command::Line([0.0; 2])],
            vec![Command::Close],
            vec![Command::Move([f32::NAN, 0.0])],
            vec![Command::Move([1e7, 0.0])],
            vec![Command::Move([0.0; 2]); 4097],
        ] {
            assert!(PathData::new(&commands, fill(), None).is_err());
        }
        assert!(
            PathData::new(
                &[Command::Move([0.0; 2])],
                None,
                Some(Stroke {
                    color: [1.0; 4],
                    width: -1.0,
                    cap: LineCap::Butt,
                    join: LineJoin::Miter
                })
            )
            .is_err()
        );
    }
    #[test]
    fn curved_input_is_bounded_before_tessellation() {
        let commands: Vec<_> = (0..256)
            .flat_map(|_| {
                [
                    Command::Move([0.0, 0.0]),
                    Command::Cubic([1e6, 0.0], [0.0, 1e6], [1e6, 1e6]),
                ]
            })
            .collect();
        let error = PathData::new(&commands, fill(), None).expect_err("segment limit");
        assert!(error.to_string().contains("flattened segment"));
    }
    #[test]
    fn geometry_output_limits_allocation() {
        let mut output = Output {
            vertices: vec![],
            indices: vec![],
            color: [1.0; 4],
            overflow: false,
        };
        for _ in 0..MAX_VERTICES {
            assert!(output.vertex([0.0; 2]).is_ok());
        }
        assert!(output.vertex([0.0; 2]).is_err());
        for _ in 0..MAX_INDICES / 3 + 1 {
            output.add_triangle(VertexId(0), VertexId(0), VertexId(0));
        }
        assert!(output.overflow);
        assert_eq!(output.indices.len(), MAX_INDICES);
    }
}
