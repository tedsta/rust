// Copyright 2012-2015 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use rustc::hir::map as ast_map;

use rustc::hir::intravisit::{Visitor, IdRangeComputingVisitor, IdRange};

use cstore::CrateMetadata;
use encoder::EncodeContext;
use schema::*;

use rustc::middle::cstore::{InlinedItem, InlinedItemRef};
use rustc::middle::const_qualif::ConstQualif;
use rustc::hir::def::{self, Def};
use rustc::hir::def_id::DefId;
use rustc::ty::{self, TyCtxt, Ty};

use syntax::ast;

use rustc_serialize::Encodable;

#[derive(RustcEncodable, RustcDecodable)]
pub struct Ast<'tcx> {
    id_range: IdRange,
    item: Lazy<InlinedItem>,
    side_tables: LazySeq<(ast::NodeId, TableEntry<'tcx>)>,
}

#[derive(RustcEncodable, RustcDecodable)]
enum TableEntry<'tcx> {
    Def(Def),
    NodeType(Ty<'tcx>),
    ItemSubsts(ty::ItemSubsts<'tcx>),
    Adjustment(ty::adjustment::Adjustment<'tcx>),
    ConstQualif(ConstQualif),
}

impl<'a, 'tcx> EncodeContext<'a, 'tcx> {
    pub fn encode_inlined_item(&mut self, ii: InlinedItemRef) -> Lazy<Ast<'tcx>> {
        let mut id_visitor = IdRangeComputingVisitor::new();
        match ii {
            InlinedItemRef::Item(_, i) => id_visitor.visit_item(i),
            InlinedItemRef::TraitItem(_, ti) => id_visitor.visit_trait_item(ti),
            InlinedItemRef::ImplItem(_, ii) => id_visitor.visit_impl_item(ii),
        }

        let ii_pos = self.position();
        ii.encode(self).unwrap();

        let tables_pos = self.position();
        let tables_count = {
            let mut visitor = SideTableEncodingIdVisitor {
                ecx: self,
                count: 0,
            };
            match ii {
                InlinedItemRef::Item(_, i) => visitor.visit_item(i),
                InlinedItemRef::TraitItem(_, ti) => visitor.visit_trait_item(ti),
                InlinedItemRef::ImplItem(_, ii) => visitor.visit_impl_item(ii),
            }
            visitor.count
        };

        self.lazy(&Ast {
            id_range: id_visitor.result(),
            item: Lazy::with_position(ii_pos),
            side_tables: LazySeq::with_position_and_length(tables_pos, tables_count),
        })
    }
}

struct SideTableEncodingIdVisitor<'a, 'b: 'a, 'tcx: 'b> {
    ecx: &'a mut EncodeContext<'b, 'tcx>,
    count: usize,
}

impl<'a, 'b, 'tcx, 'v> Visitor<'v> for SideTableEncodingIdVisitor<'a, 'b, 'tcx> {
    fn visit_id(&mut self, id: ast::NodeId) {
        debug!("Encoding side tables for id {}", id);

        let tcx = self.ecx.tcx;
        let mut encode = |entry: Option<TableEntry>| {
            if let Some(entry) = entry {
                (id, entry).encode(self.ecx).unwrap();
                self.count += 1;
            }
        };

        encode(tcx.expect_def_or_none(id).map(TableEntry::Def));
        encode(tcx.tables().node_types.get(&id).cloned().map(TableEntry::NodeType));
        encode(tcx.tables().item_substs.get(&id).cloned().map(TableEntry::ItemSubsts));
        encode(tcx.tables().adjustments.get(&id).cloned().map(TableEntry::Adjustment));
        encode(tcx.const_qualif_map.borrow().get(&id).cloned().map(TableEntry::ConstQualif));
    }
}

/// Decodes an item from its AST in the cdata's metadata and adds it to the
/// ast-map.
pub fn decode_inlined_item<'a, 'tcx>(cdata: &CrateMetadata,
                                     tcx: TyCtxt<'a, 'tcx, 'tcx>,
                                     parent_def_path: ast_map::DefPath,
                                     parent_did: DefId,
                                     ast: Ast<'tcx>,
                                     orig_did: DefId)
                                     -> &'tcx InlinedItem {
    debug!("> Decoding inlined fn: {:?}", tcx.item_path_str(orig_did));

    let cnt = ast.id_range.max.as_usize() - ast.id_range.min.as_usize();
    let start = tcx.sess.reserve_node_ids(cnt);
    let id_ranges = [ast.id_range,
                     IdRange {
                         min: start,
                         max: ast::NodeId::new(start.as_usize() + cnt),
                     }];

    let ii = ast.item.decode((cdata, tcx, id_ranges));
    let ii = ast_map::map_decoded_item(&tcx.map,
                                       parent_def_path,
                                       parent_did,
                                       ii,
                                       tcx.sess.next_node_id());

    let item_node_id = match ii {
        &InlinedItem::Item(_, ref i) => i.id,
        &InlinedItem::TraitItem(_, ref ti) => ti.id,
        &InlinedItem::ImplItem(_, ref ii) => ii.id,
    };
    let inlined_did = tcx.map.local_def_id(item_node_id);
    tcx.register_item_type(inlined_did, tcx.lookup_item_type(orig_did));

    for (id, entry) in ast.side_tables.decode((cdata, tcx, id_ranges)) {
        match entry {
            TableEntry::Def(def) => {
                tcx.def_map.borrow_mut().insert(id, def::PathResolution::new(def));
            }
            TableEntry::NodeType(ty) => {
                tcx.node_type_insert(id, ty);
            }
            TableEntry::ItemSubsts(item_substs) => {
                tcx.tables.borrow_mut().item_substs.insert(id, item_substs);
            }
            TableEntry::Adjustment(adj) => {
                tcx.tables.borrow_mut().adjustments.insert(id, adj);
            }
            TableEntry::ConstQualif(qualif) => {
                tcx.const_qualif_map.borrow_mut().insert(id, qualif);
            }
        }
    }

    ii
}
