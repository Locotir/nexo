//! Reading the functions, variables and types out of `.debug_info`.

use super::*;

impl Vars {
    /// The variables of an image, or `None` when it has no (readable) DWARF.
    pub fn of(img: &Image) -> Option<Vars> {
        // the DWARF reader trusts address arithmetic that a damaged file can overflow (a panic in debug builds);
        // debug info it cannot read is "no debug info", not a crash
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| Self::read(img))).ok().flatten()
    }

    fn read(img: &Image) -> Option<Vars> {
        let get = |name: &str| -> &[u8] {
            img.sections.iter().find(|s| s.name == name).map_or(&[][..], |s| img.section_bytes(s))
        };
        if get(".debug_info").is_empty() || get(".debug_info").len() > MAX_BYTES {
            return None;
        }
        let load = |id: gimli::SectionId| -> std::result::Result<Slice<'_>, gimli::Error> {
            Ok(EndianSlice::new(get(id.name()), LittleEndian))
        };
        let dwarf = gimli::Dwarf::load(load).ok()?;
        let mut out = Vars { funcs: Vec::new(), globals: Vec::new(), types: HashMap::new() };
        let mut units = dwarf.units();
        while let Ok(Some(header)) = units.next() {
            let Ok(unit) = dwarf.unit(header) else { continue };
            out.read_unit(&dwarf, &unit);
        }
        Some(out)
    }

    fn read_unit<'a>(&mut self, dwarf: &gimli::Dwarf<Slice<'a>>, unit: &gimli::Unit<Slice<'a>>) {
        let enc = unit.encoding();
        let name = |e: &gimli::DebuggingInformationEntry<Slice<'a>>| -> String {
            e.attr_value(gimli::DW_AT_name)
                .and_then(|v| dwarf.attr_string(unit, v).ok())
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        let type_of = |e: &gimli::DebuggingInformationEntry<Slice<'a>>| -> Option<u64> {
            match e.attr_value(gimli::DW_AT_type)? {
                AttributeValue::UnitRef(o) => o.to_debug_info_offset(&unit.header).map(|o| o.0 as u64),
                AttributeValue::DebugInfoRef(o) => Some(o.0 as u64),
                _ => None,
            }
        };
        let ranges = |e: &gimli::DebuggingInformationEntry<Slice<'a>>| -> Vec<(u64, u64)> {
            let mut v = Vec::new();
            if let Ok(mut r) = dwarf.die_ranges(unit, e) {
                while let Ok(Some(r)) = r.next() {
                    if r.end > r.begin {
                        v.push((r.begin, r.end));
                    }
                }
            }
            v
        };
        let mut stack: Vec<(isize, Ctx)> = Vec::new();
        let mut entries = unit.entries();
        while let Ok(Some(e)) = entries.next_dfs() {
            let d = e.depth();
            while stack.last().is_some_and(|(sd, _)| *sd >= d) {
                stack.pop();
            }
            let off = e.offset().to_debug_info_offset(&unit.header).map_or(0, |o| o.0 as u64);
            let size = e.attr_value(gimli::DW_AT_byte_size).and_then(|v| v.udata_value()).unwrap_or(0);
            let ctx = match e.tag() {
                gimli::DW_TAG_subprogram => {
                    let r = ranges(e);
                    if r.is_empty() || stack.iter().any(|(_, c)| matches!(c, Ctx::Skip)) {
                        Ctx::Skip
                    } else {
                        let frame_base = e.attr_value(gimli::DW_AT_frame_base).and_then(|v| v.exprloc_value());
                        self.funcs.push(Func {
                            name: name(e),
                            ranges: r,
                            frame_base: frame_base.map(|x| x.0.slice().to_vec()),
                            enc,
                            vars: Vec::new(),
                        });
                        Ctx::Func(self.funcs.len() - 1)
                    }
                }
                gimli::DW_TAG_lexical_block => Ctx::Block(ranges(e)),
                gimli::DW_TAG_inlined_subroutine => Ctx::Skip,
                t @ (gimli::DW_TAG_formal_parameter | gimli::DW_TAG_variable) => {
                    let loc = match e.attr_value(gimli::DW_AT_location) {
                        Some(AttributeValue::Exprloc(x)) => Loc::Expr(x.0.slice().to_vec()),
                        Some(v) => match dwarf.attr_locations(unit, v) {
                            Ok(Some(mut it)) => {
                                let mut l = Vec::new();
                                while let Ok(Some(x)) = it.next() {
                                    l.push((x.range.begin, x.range.end, x.data.0.slice().to_vec()));
                                }
                                Loc::List(l)
                            }
                            _ => Loc::None,
                        },
                        None => Loc::None,
                    };
                    let mut var = Var {
                        name: name(e),
                        param: t == gimli::DW_TAG_formal_parameter,
                        loc,
                        ty: type_of(e),
                        scope: None,
                    };
                    let mut owner = None;
                    for (_, c) in stack.iter().rev() {
                        match c {
                            Ctx::Skip | Ctx::Type(_) => break,
                            Ctx::Block(r) if var.scope.is_none() => var.scope = Some(r.clone()),
                            Ctx::Func(i) => {
                                owner = Some(Some(*i));
                                break;
                            }
                            _ => {}
                        }
                    }
                    if stack.is_empty() || stack.iter().all(|(_, c)| matches!(c, Ctx::Other)) {
                        owner = Some(None);
                    }
                    match owner {
                        Some(Some(i)) if !var.name.is_empty() => self.funcs[i].vars.push(var),
                        Some(None) if !var.name.is_empty() && !matches!(var.loc, Loc::None) => {
                            self.globals.push((var, enc))
                        }
                        _ => {}
                    }
                    Ctx::Other
                }
                gimli::DW_TAG_base_type => {
                    let enc = match e.attr_value(gimli::DW_AT_encoding) {
                        Some(AttributeValue::Encoding(x)) => x.0,
                        _ => 0,
                    };
                    self.types.insert(off, Ty::Base { name: name(e), size, enc });
                    Ctx::Other
                }
                gimli::DW_TAG_pointer_type | gimli::DW_TAG_reference_type | gimli::DW_TAG_rvalue_reference_type => {
                    self.types.insert(off, Ty::Ptr { to: type_of(e) });
                    Ctx::Other
                }
                gimli::DW_TAG_array_type => {
                    self.types.insert(off, Ty::Array { elem: type_of(e), dims: Vec::new() });
                    Ctx::Type(off)
                }
                gimli::DW_TAG_subrange_type => {
                    let n = match e.attr_value(gimli::DW_AT_count).and_then(|v| v.udata_value()) {
                        Some(n) => Some(n),
                        None => e.attr_value(gimli::DW_AT_upper_bound).and_then(|v| v.udata_value()).map(|u| u + 1),
                    };
                    if let Some((_, Ctx::Type(a))) = stack.last() {
                        if let Some(Ty::Array { dims, .. }) = self.types.get_mut(a) {
                            dims.push(n);
                        }
                    }
                    Ctx::Other
                }
                t @ (gimli::DW_TAG_structure_type | gimli::DW_TAG_class_type | gimli::DW_TAG_union_type) => {
                    let union = t == gimli::DW_TAG_union_type;
                    self.types.insert(off, Ty::Record { name: name(e), size, members: Vec::new(), union });
                    Ctx::Type(off)
                }
                gimli::DW_TAG_member => {
                    let at = match e.attr_value(gimli::DW_AT_data_member_location) {
                        Some(v) => v.udata_value().unwrap_or(0),
                        None => 0,
                    };
                    let m = (name(e), at, type_of(e));
                    if let Some((_, Ctx::Type(r))) = stack.last() {
                        if let Some(Ty::Record { members, .. }) = self.types.get_mut(r) {
                            members.push(m);
                        }
                    }
                    Ctx::Other
                }
                gimli::DW_TAG_typedef => {
                    self.types.insert(off, Ty::Alias { name: name(e), to: type_of(e) });
                    Ctx::Other
                }
                gimli::DW_TAG_const_type
                | gimli::DW_TAG_volatile_type
                | gimli::DW_TAG_restrict_type
                | gimli::DW_TAG_atomic_type => {
                    self.types.insert(off, Ty::Alias { name: String::new(), to: type_of(e) });
                    Ctx::Other
                }
                gimli::DW_TAG_enumeration_type => {
                    self.types.insert(off, Ty::Enum { name: name(e), size, vals: Vec::new() });
                    Ctx::Type(off)
                }
                gimli::DW_TAG_enumerator => {
                    let v = e.attr_value(gimli::DW_AT_const_value);
                    let v = v.as_ref().and_then(|v| v.sdata_value().or_else(|| v.udata_value().map(|u| u as i64)));
                    if let (Some((_, Ctx::Type(t))), Some(v)) = (stack.last(), v) {
                        if let Some(Ty::Enum { vals, .. }) = self.types.get_mut(t) {
                            vals.push((name(e), v));
                        }
                    }
                    Ctx::Other
                }
                gimli::DW_TAG_subroutine_type => {
                    self.types.insert(off, Ty::Func);
                    Ctx::Skip
                }
                gimli::DW_TAG_compile_unit | gimli::DW_TAG_namespace => Ctx::Other,
                _ => Ctx::Skip,
            };
            stack.push((d, ctx));
        }
    }
}
