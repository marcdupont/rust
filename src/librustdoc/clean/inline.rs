// Copyright 2012-2013 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Support for inlining external documentation into the current AST.

use syntax::ast;
use syntax::ast_util;
use syntax::attr::AttrMetaMethods;

use rustc::metadata::csearch;
use rustc::metadata::decoder;
use rustc::middle::ty;

use core;
use doctree;
use clean;

use super::Clean;

/// Attempt to inline the definition of a local node id into this AST.
///
/// This function will fetch the definition of the id specified, and if it is
/// from another crate it will attempt to inline the documentation from the
/// other crate into this crate.
///
/// This is primarily used for `pub use` statements which are, in general,
/// implementation details. Inlining the documentation should help provide a
/// better experience when reading the documentation in this use case.
///
/// The returned value is `None` if the `id` could not be inlined, and `Some`
/// of a vector of items if it was successfully expanded.
pub fn try_inline(id: ast::NodeId) -> Option<Vec<clean::Item>> {
    let cx = ::ctxtkey.get().unwrap();
    let tcx = match cx.maybe_typed {
        core::Typed(ref tycx) => tycx,
        core::NotTyped(_) => return None,
    };
    let def = match tcx.def_map.borrow().find(&id) {
        Some(def) => *def,
        None => return None,
    };
    let did = ast_util::def_id_of_def(def);
    if ast_util::is_local(did) { return None }
    try_inline_def(&**cx, tcx, def)
}

fn try_inline_def(cx: &core::DocContext,
                  tcx: &ty::ctxt,
                  def: ast::Def) -> Option<Vec<clean::Item>> {
    let mut ret = Vec::new();
    let did = ast_util::def_id_of_def(def);
    let inner = match def {
        ast::DefTrait(did) => {
            record_extern_fqn(cx, did, clean::TypeTrait);
            clean::TraitItem(build_external_trait(tcx, did))
        }
        ast::DefFn(did, style) => {
            record_extern_fqn(cx, did, clean::TypeFunction);
            clean::FunctionItem(build_external_function(tcx, did, style))
        }
        ast::DefStruct(did) => {
            record_extern_fqn(cx, did, clean::TypeStruct);
            ret.extend(build_impls(tcx, did).move_iter());
            clean::StructItem(build_struct(tcx, did))
        }
        ast::DefTy(did) => {
            record_extern_fqn(cx, did, clean::TypeEnum);
            ret.extend(build_impls(tcx, did).move_iter());
            build_type(tcx, did)
        }
        // Assume that the enum type is reexported next to the variant, and
        // variants don't show up in documentation specially.
        ast::DefVariant(..) => return Some(Vec::new()),
        ast::DefMod(did) => {
            record_extern_fqn(cx, did, clean::TypeModule);
            clean::ModuleItem(build_module(cx, tcx, did))
        }
        _ => return None,
    };
    let fqn = csearch::get_item_path(tcx, did);
    ret.push(clean::Item {
        source: clean::Span::empty(),
        name: Some(fqn.last().unwrap().to_str().to_strbuf()),
        attrs: load_attrs(tcx, did),
        inner: inner,
        visibility: Some(ast::Public),
        def_id: did,
    });
    Some(ret)
}

pub fn load_attrs(tcx: &ty::ctxt, did: ast::DefId) -> Vec<clean::Attribute> {
    let mut attrs = Vec::new();
    csearch::get_item_attrs(&tcx.sess.cstore, did, |v| {
        attrs.extend(v.move_iter().map(|mut a| {
            // FIXME this isn't quite always true, it's just true about 99% of
            //       the time when dealing with documentation. For example,
            //       this would treat doc comments of the form `#[doc = "foo"]`
            //       incorrectly.
            if a.name().get() == "doc" && a.value_str().is_some() {
                a.node.is_sugared_doc = true;
            }
            a.clean()
        }));
    });
    attrs
}

/// Record an external fully qualified name in the external_paths cache.
///
/// These names are used later on by HTML rendering to generate things like
/// source links back to the original item.
pub fn record_extern_fqn(cx: &core::DocContext,
                         did: ast::DefId,
                         kind: clean::TypeKind) {
    match cx.maybe_typed {
        core::Typed(ref tcx) => {
            let fqn = csearch::get_item_path(tcx, did);
            let fqn = fqn.move_iter().map(|i| i.to_str().to_strbuf()).collect();
            cx.external_paths.borrow_mut().get_mut_ref().insert(did, (fqn, kind));
        }
        core::NotTyped(..) => {}
    }
}

pub fn build_external_trait(tcx: &ty::ctxt, did: ast::DefId) -> clean::Trait {
    let def = ty::lookup_trait_def(tcx, did);
    let methods = ty::trait_methods(tcx, did);
    clean::Trait {
        generics: def.generics.clean(),
        methods: methods.iter().map(|i| i.clean()).collect(),
        parents: Vec::new(), // FIXME: this is likely wrong
    }
}

fn build_external_function(tcx: &ty::ctxt,
                           did: ast::DefId,
                           style: ast::FnStyle) -> clean::Function {
    let t = ty::lookup_item_type(tcx, did);
    clean::Function {
        decl: match ty::get(t.ty).sty {
            ty::ty_bare_fn(ref f) => (did, &f.sig).clean(),
            _ => fail!("bad function"),
        },
        generics: t.generics.clean(),
        fn_style: style,
    }
}

fn build_struct(tcx: &ty::ctxt, did: ast::DefId) -> clean::Struct {
    use syntax::parse::token::special_idents::unnamed_field;

    let t = ty::lookup_item_type(tcx, did);
    let fields = ty::lookup_struct_fields(tcx, did);

    clean::Struct {
        struct_type: match fields.as_slice() {
            [] => doctree::Unit,
            [ref f] if f.name == unnamed_field.name => doctree::Newtype,
            [ref f, ..] if f.name == unnamed_field.name => doctree::Tuple,
            _ => doctree::Plain,
        },
        generics: t.generics.clean(),
        fields: fields.iter().map(|f| f.clean()).collect(),
        fields_stripped: false,
    }
}

fn build_type(tcx: &ty::ctxt, did: ast::DefId) -> clean::ItemEnum {
    let t = ty::lookup_item_type(tcx, did);
    match ty::get(t.ty).sty {
        ty::ty_enum(edid, _) => {
            return clean::EnumItem(clean::Enum {
                generics: t.generics.clean(),
                variants_stripped: false,
                variants: ty::enum_variants(tcx, edid).clean(),
            })
        }
        _ => {}
    }

    clean::TypedefItem(clean::Typedef {
        type_: t.ty.clean(),
        generics: t.generics.clean(),
    })
}

fn build_impls(tcx: &ty::ctxt,
               did: ast::DefId) -> Vec<clean::Item> {
    ty::populate_implementations_for_type_if_necessary(tcx, did);
    let mut impls = Vec::new();

    match tcx.inherent_impls.borrow().find(&did) {
        None => {}
        Some(i) => {
            impls.extend(i.borrow().iter().map(|&did| { build_impl(tcx, did) }));
        }
    }

    impls
}

fn build_impl(tcx: &ty::ctxt, did: ast::DefId) -> clean::Item {
    let associated_trait = csearch::get_impl_trait(tcx, did);
    let attrs = load_attrs(tcx, did);
    let ty = ty::lookup_item_type(tcx, did);
    let methods = csearch::get_impl_methods(&tcx.sess.cstore, did).iter().map(|did| {
        let mut item = match ty::method(tcx, *did).clean() {
            clean::Provided(item) => item,
            clean::Required(item) => item,
        };
        item.inner = match item.inner.clone() {
            clean::TyMethodItem(clean::TyMethod {
                fn_style, decl, self_, generics
            }) => {
                clean::MethodItem(clean::Method {
                    fn_style: fn_style,
                    decl: decl,
                    self_: self_,
                    generics: generics,
                })
            }
            _ => fail!("not a tymethod"),
        };
        item
    }).collect();
    clean::Item {
        inner: clean::ImplItem(clean::Impl {
            derived: clean::detect_derived(attrs.as_slice()),
            trait_: associated_trait.clean().map(|bound| {
                match bound {
                    clean::TraitBound(ty) => ty,
                    clean::RegionBound => unreachable!(),
                }
            }),
            for_: ty.ty.clean(),
            generics: ty.generics.clean(),
            methods: methods,
        }),
        source: clean::Span::empty(),
        name: None,
        attrs: attrs,
        visibility: Some(ast::Inherited),
        def_id: did,
    }
}

fn build_module(cx: &core::DocContext, tcx: &ty::ctxt,
                did: ast::DefId) -> clean::Module {
    let mut items = Vec::new();

    // FIXME: this doesn't handle reexports inside the module itself.
    //        Should they be handled?
    csearch::each_child_of_item(&tcx.sess.cstore, did, |def, _, _| {
        match def {
            decoder::DlDef(def) => {
                match try_inline_def(cx, tcx, def) {
                    Some(i) => items.extend(i.move_iter()),
                    None => {}
                }
            }
            decoder::DlImpl(did) => items.push(build_impl(tcx, did)),
            decoder::DlField => fail!("unimplemented field"),
        }
    });

    clean::Module {
        items: items,
        is_crate: false,
    }
}
