use std::collections::{BTreeMap, BTreeSet, HashMap};

mod segment;
use annotation::{Annotation, AnnotationId, Opts};
use anomaly_fixer::{apply_fixup, fixup_byte_to_char};
use formatting::Text;
use range_map::{Range, RangeSet};
use segment::{Segment, SegmentBuffer};
use single_line::LineAnnotation;

use crate::formatting::Formatting;

mod annotation;
mod anomaly_fixer;
mod chars;
mod formatting;
mod single_line;

#[derive(Clone)]
struct RawLine {
	data: Text,
}

struct AnnotationLine {
	prefix: Text,
	line: Text,
	/// There will be lines drawn to connect lines with the same annotation id specified
	annotation: Option<AnnotationId>,
}

struct GapLine {
	prefix: Text,
	line: Text,
}

struct TextLine {
	prefix: Text,
	line_num: usize,
	line: Text,
	annotations: Vec<LineAnnotation>,
	annotation_buffers: Vec<(Option<AnnotationId>, Text)>,
}
impl TextLine {
	fn add_prefix(&mut self, this: Text, annotations: Text) {
		self.prefix.extend(this);
		for (_, ele) in self.annotation_buffers.iter_mut() {
			ele.splice(0..0, Some(annotations.clone()));
		}
	}
	fn len(&self) -> usize {
		self.line.len()
	}
	fn is_empty(&self) -> bool {
		self.line.is_empty()
	}
	// fn trim_end(&mut self) {
	// 	self.line.truncate(self.line.trim_end().len());
	// }
}

fn cons_slices<T>(mut slice: &mut [T], test: impl Fn(&T) -> bool) -> Vec<&mut [T]> {
	let mut out = Vec::new();

	while !slice.is_empty() {
		let mut skip = 0;
		while !slice.get(skip).map(&test).unwrap_or(true) {
			skip += 1;
		}
		let mut take = 0;
		while slice.get(skip + take).map(&test).unwrap_or(false) {
			take += 1;
		}
		let (_skipped, rest) = slice.split_at_mut(skip);
		let (taken, rest) = rest.split_at_mut(take);
		if !taken.is_empty() {
			out.push(taken);
		}
		slice = rest;
	}

	out
}

enum Line {
	Text(TextLine),
	Annotation(AnnotationLine),
	Raw(RawLine),
	Nop,
	Gap(GapLine),
}
impl Line {
	fn text_mut(&mut self) -> Option<&mut Text> {
		Some(match self {
			Line::Text(t) => &mut t.line,
			Line::Gap(t) => &mut t.line,
			Line::Annotation(t) => &mut t.line,
			_ => return None,
		})
	}
	fn is_text(&self) -> bool {
		matches!(self, Self::Text(_))
	}
	fn is_annotation(&self) -> bool {
		matches!(self, Self::Annotation(_))
	}
	fn as_annotation(&self) -> Option<&AnnotationLine> {
		match self {
			Self::Annotation(a) => Some(a),
			_ => None,
		}
	}
	fn is_gap(&self) -> bool {
		matches!(self, Self::Gap(_))
	}
	fn as_text_mut(&mut self) -> Option<&mut TextLine> {
		match self {
			Line::Text(t) => Some(t),
			_ => None,
		}
	}
	fn as_gap_mut(&mut self) -> Option<&mut GapLine> {
		match self {
			Line::Gap(t) => Some(t),
			_ => None,
		}
	}
	fn as_text(&self) -> Option<&TextLine> {
		match self {
			Line::Text(t) => Some(t),
			_ => None,
		}
	}
	fn as_raw(&self) -> Option<&RawLine> {
		match self {
			Line::Raw(r) => Some(r),
			_ => None,
		}
	}
	fn is_nop(&self) -> bool {
		matches!(self, Self::Nop)
	}
}

pub struct Source {
	lines: Vec<Line>,
}

fn cleanup_nops(source: &mut Source) {
	let mut i = 0;
	while i < source.lines.len() {
		if source.lines[i].is_nop() {
			source.lines.remove(i);
		} else {
			i += 1;
		}
	}
}

/// Remove NOP/empty annotation lines
fn cleanup(source: &mut Source) {
	for slice in cons_slices(&mut source.lines, Line::is_text) {
		for line in slice
			.iter_mut()
			.take_while(|l| l.as_text().unwrap().is_empty())
		{
			*line = Line::Nop;
		}
		for line in slice
			.iter_mut()
			.rev()
			.take_while(|l| l.as_text().unwrap().is_empty())
		{
			*line = Line::Nop;
		}
	}
	cleanup_nops(source);
	for slice in cons_slices(&mut source.lines, Line::is_gap) {
		if slice.len() == 1 {
			continue;
		}
		for ele in slice.iter_mut().skip(1) {
			*ele = Line::Nop;
		}
	}
	cleanup_nops(source);
}

fn process(
	source: &mut Source,
	annotation_formats: HashMap<AnnotationId, Formatting>,
	opts: &Opts,
) {
	cleanup(source);
	// Format inline annotations
	{
		for line in source
			.lines
			.iter_mut()
			.flat_map(Line::as_text_mut)
			.filter(|t| !t.annotations.is_empty())
		{
			let (replace, extra) =
				single_line::generate_segment(line.annotations.clone(), line.line.clone(), opts);
			line.line = replace;
			line.annotation_buffers = extra;
			line.annotations.truncate(0);
		}
	}
	// Make gaps in files
	for slice in cons_slices(&mut source.lines, Line::is_text) {
		'line: for i in 0..slice.len() {
			for j in i.saturating_sub(2)..(i + 3) {
				let Some(ctx) = slice.get(j) else {
					continue;
				};
				let Line::Text(t) = ctx else {
					continue;
				};
				if t.annotation_buffers.is_empty() {
					continue;
				}
				continue 'line;
			}
			slice[i] = Line::Gap(GapLine {
				prefix: Text::new([]),
				line: Text::new([]),
			});
		}
	}
	cleanup(source);

	// Expand annotation buffers
	{
		let mut insertions = vec![];
		for (i, line) in source
			.lines
			.iter_mut()
			.enumerate()
			.flat_map(|(i, l)| l.as_text_mut().map(|t| (i, t)))
		{
			for buf in line.annotation_buffers.drain(..) {
				insertions.push((i + 1, buf))
			}
		}
		insertions.reverse();
		for (i, (annotation, line)) in insertions {
			source.lines.insert(
				i,
				Line::Annotation(AnnotationLine {
					line,
					annotation,
					prefix: SegmentBuffer::new([]),
				}),
			);
		}
	}
	// Connect annotation lines
	{
		for lines in &mut cons_slices(&mut source.lines, |l| {
			l.is_annotation() || l.is_text() || l.is_gap()
		}) {
			struct Connection {
				range: Range<usize>,
				connected: Vec<usize>,
			}

			let mut connected_annotations = HashMap::new();
			for (i, line) in lines.iter().enumerate() {
				if let Some(annotation) = line.as_annotation() {
					if let Some(annotation) = annotation.annotation {
						let conn = connected_annotations
							.entry(annotation)
							.or_insert(Connection {
								range: Range::new(i, i),
								connected: Vec::new(),
							});
						conn.range.start = conn.range.start.min(i);
						conn.range.end = conn.range.end.max(i);
						conn.connected.push(i);
					}
				}
			}
			let mut grouped = connected_annotations
				.iter()
				.map(|(k, v)| (*k, vec![v.range].into_iter().collect::<RangeSet<usize>>()))
				.collect::<Vec<_>>();
			grouped.sort_by_key(|a| a.1.num_elements());
			let grouped = single_line::group_nonconflicting(grouped);

			for group in grouped {
				for annotation in group {
					let annotation_fmt = annotation_formats
						.get(&annotation)
						.expect("id is used in string but not defined")
						.clone()
						.decoration();
					let conn = connected_annotations.get(&annotation).expect("exists");
					let range = conn.range;
					let mut max_index = usize::MAX;
					for line in range.start..=range.end {
						match &lines[line] {
							Line::Text(t) if t.line.data().all(|c| c.is_whitespace()) => {}
							Line::Text(t) => {
								let whitespaces =
									t.line.data().take_while(|i| i.is_whitespace()).count();
								max_index = max_index.min(whitespaces)
							}
							Line::Annotation(t) if t.line.data().all(|c| c.is_whitespace()) => {}
							Line::Annotation(t) => {
								let whitespaces =
									t.line.data().take_while(|i| i.is_whitespace()).count();
								max_index = max_index.min(whitespaces)
							}
							Line::Gap(_) => {}
							_ => unreachable!(),
						}
					}
					while max_index < 2 {
						let seg = Some(SegmentBuffer::new([Segment::new(
							vec![' '; 2 - max_index],
							annotation_fmt.clone(),
						)]));
						for line in lines.iter_mut() {
							match line {
								Line::Text(t) => t.line.splice(0..0, seg.clone()),
								Line::Annotation(t) => t.line.splice(0..0, seg.clone()),
								Line::Gap(t) => t.line.splice(0..0, seg.clone()),
								_ => unreachable!(),
							}
						}
						max_index = 2;
					}
					if max_index >= 2 {
						let offset = max_index - 2;

						for line in range.start..=range.end {
							use chars::line::*;
							let char = if range.start == range.end {
								RANGE_EMPTY
							} else if line == range.start {
								RANGE_START
							} else if line == range.end {
								RANGE_END
							} else if conn.connected.contains(&line) {
								RANGE_CONNECTION
							} else {
								RANGE_CONTINUE
							};
							let text = lines[line].text_mut().expect("only with text reachable");
							if text.len() <= offset {
								text.resize(offset + 1, ' ', annotation_fmt.clone());
							}
							text.splice(
								offset..=offset,
								Some(SegmentBuffer::new([Segment::new(
									[char],
									annotation_fmt.clone(),
								)])),
							);

							if conn.connected.contains(&line) {
								for i in offset + 1..text.len() {
									let (char, fmt) = text.get(i).expect("in bounds");
									if !text.get(i).expect("in bounds").0.is_whitespace()
										&& !fmt.decoration
									{
										break;
									}
									if let Some((keep_style, replacement)) = cross(char) {
										text.splice(
											i..=i,
											Some(SegmentBuffer::new([Segment::new(
												[replacement],
												if keep_style {
													fmt
												} else {
													annotation_fmt.clone()
												},
											)])),
										)
									}
								}
							}
						}
					}
				}
			}
		}
	}
	// Apply line numbers
	{
		for lines in &mut cons_slices(&mut source.lines, |l| {
			l.is_annotation() || l.is_text() || l.is_gap()
		}) {
			let max_num = lines
				.iter()
				.filter_map(|l| match l {
					Line::Text(t) => Some(t.line_num),
					_ => None,
				})
				.max()
				.unwrap_or(0);
			let max_len = max_num.to_string().len();
			let prefix_segment = Segment::new(vec![' '; max_len - 1], Formatting::line_number());
			for line in lines.iter_mut() {
				match line {
					Line::Text(t) => t.prefix.extend(SegmentBuffer::new([Segment::new(
						format!("{:>width$} ", t.line_num, width = max_len).chars(),
						Formatting::line_number(),
					)])),
					Line::Annotation(a) => a.prefix.extend(SegmentBuffer::new([
						prefix_segment.clone(),
						Segment::new(['·', ' '], Formatting::line_number()),
					])),
					Line::Gap(a) => a.prefix.extend(SegmentBuffer::new([
						prefix_segment.clone(),
						Segment::new(['⋮', ' '], Formatting::line_number()),
					])),
					_ => unreachable!(),
				}
			}
		}
	}
	// To raw
	{
		for line in &mut source.lines {
			match line {
				Line::Text(t) => {
					let mut buf = SegmentBuffer::new([]);
					buf.extend(t.prefix.clone());
					buf.extend(t.line.clone());
					*line = Line::Raw(RawLine { data: buf });
				}
				Line::Annotation(t) => {
					let mut buf = SegmentBuffer::new([]);
					buf.extend(t.prefix.clone());
					buf.extend(t.line.clone());
					*line = Line::Raw(RawLine { data: buf })
				}
				Line::Gap(t) => {
					let mut buf = SegmentBuffer::new([]);
					buf.extend(t.prefix.clone());
					buf.extend(t.line.clone());
					*line = Line::Raw(RawLine { data: buf })
				}
				Line::Raw(_) | Line::Nop => {}
			}
		}
	}
	cleanup(source);
}

fn linestarts(str: &str) -> BTreeSet<usize> {
	let mut linestarts = BTreeSet::new();
	for (i, c) in str.chars().enumerate() {
		if c == '\n' {
			linestarts.insert(i + 1);
		}
	}
	linestarts
}
struct LineCol {
	line: usize,
	column: usize,
}
fn offset_to_linecol(mut offset: usize, linestarts: &BTreeSet<usize>) -> LineCol {
	let mut line = 0;
	let last_offset = linestarts
		.range(..=offset)
		.inspect(|_| line += 1)
		.last()
		.copied()
		.unwrap_or(0);
	offset -= last_offset;
	LineCol {
		line,
		column: offset,
	}
}

pub fn parse(txt: &str, annotations: &[Annotation], opts: &Opts) -> Source {
	let (txt, byte_to_char_fixup) = fixup_byte_to_char(txt, "    ");
	let mut annotations = annotations.to_vec();

	// Convert byte offsets to char offsets
	for annotation in annotations.iter_mut() {
		let ranges: RangeSet<usize> = annotation
			.ranges
			.ranges()
			.map(|r| {
				let mut start = r.start;
				let mut end = r.end;
				apply_fixup(&mut start, &byte_to_char_fixup);
				apply_fixup(&mut end, &byte_to_char_fixup);
				Range::new(start, end)
			})
			.collect();
		annotation.ranges = ranges;
	}
	let linestarts = linestarts(&txt);

	let mut lines: Vec<Line> = txt
		.split('\n')
		.map(|s| s.to_string())
		.enumerate()
		.map(|(num, line)| TextLine {
			line_num: num + 1,
			line: SegmentBuffer::new([Segment::new(
				// Reserve 1 char for the spans pointing to EOL
				line.chars().chain([' '].into_iter()),
				Formatting::default(),
			)]),
			prefix: SegmentBuffer::new([]),
			annotations: Vec::new(),
			annotation_buffers: Vec::new(),
		})
		.map(Line::Text)
		.collect();

	for annotation in &annotations {
		let mut line_ranges: BTreeMap<usize, RangeSet<usize>> = BTreeMap::new();
		for range in annotation.ranges.ranges() {
			let start = offset_to_linecol(range.start, &linestarts);
			let end = offset_to_linecol(range.end, &linestarts);

			if start.line == end.line {
				let set = line_ranges.entry(start.line).or_insert_with(RangeSet::new);
				*set = set.union(&[Range::new(start.column, end.column)].into_iter().collect());
			} else {
				{
					let set = line_ranges.entry(start.line).or_insert_with(RangeSet::new);
					let line = lines[start.line].as_text().expect("annotation OOB");
					*set = set.union(
						&[Range::new(start.column, line.len() - 1)]
							.into_iter()
							.collect(),
					);
				}
				{
					let set = line_ranges.entry(end.line).or_insert_with(RangeSet::new);
					*set = set.union(&[Range::new(0, end.column)].into_iter().collect());
				}
			}
		}
		let left = line_ranges.len() > 1;
		let line_ranges_len = line_ranges.len();

		for (i, (line, ranges)) in line_ranges.into_iter().enumerate() {
			let last = i == line_ranges_len - 1;
			let line = lines[line].as_text_mut().expect("annotation OOB");
			line.annotations.push(LineAnnotation {
				id: annotation.id,
				priority: annotation.priority,
				ranges,
				formatting: annotation.formatting.clone(),
				left,
				right: if last {
					annotation.text.clone()
				} else {
					Text::empty()
				},
			})
		}
	}

	let mut source = Source { lines };

	let annotation_formats = annotations
		.iter()
		.map(|a| (a.id, a.formatting.clone()))
		.collect();

	process(&mut source, annotation_formats, opts);

	source
}

fn source_to_ansi(source: &Source) -> String {
	let mut out = String::new();
	for line in &source.lines {
		let line = line
			.as_raw()
			.expect("after processing all lines should turn raw");
		let mut data = line.data.clone();
		data.compact();
		formatting::text_to_ansi(&data, &mut out);
		out.push('\n');
	}
	out
}

#[cfg(test)]
mod tests {
	use crate::annotation::AnnotationIdAllocator;

	use super::*;

	fn default<T: Default>() -> T {
		Default::default()
	}

	#[test]
	fn test_fmt() {
		use range_map::Range;
		let mut aid = AnnotationIdAllocator::new();
		let mut annotation_formats = HashMap::new();

		let s = {
			let id = aid.next();
			annotation_formats.insert(id, Formatting::color(0xffffff00));
			parse(
				include_str!("../../../fixtures/std.jsonnet"),
				&[Annotation {
					id,
					priority: 0,
					formatting: Formatting::color(0xffffff00),
					ranges: [Range::new(2832, 3135)].into_iter().collect(),
					text: Text::single("Hello world".chars(), default()),
				}],
				&Opts {
					first_layer_reformats_orig: true,
					..default()
				},
			)
		};

		println!("{}", source_to_ansi(&s))
	}
}
