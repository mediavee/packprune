use std::collections::{BTreeMap, BTreeSet};

use packprune_graph::{
    SourceError,
    UsageKind::{Equipment, ItemDefinition, Model, Sprite},
};
use packprune_pack::ResourceId;

use super::{
    Claims,
    keys::{
        self, ARMOR, ARMOR_ID, ARMOR_LAYERS, CUSTOM_MODEL_DATA, DEFAULT_MATERIAL, Form,
        GENERATED_NAMESPACE, ITEM_MODEL_COMPONENT, ITEM_MODEL_SECTION, MATERIAL, MECHANICS,
        NEXO_ITEM, PACK, PACK_BBMODEL, PACK_MODEL, PACK_TEXTURES, TEMPLATE,
    },
};
use crate::{document::Node, rules::usage};

pub(super) fn read(
    files: &[(String, Node)],
    recipes: &[(String, Node)],
    prefer: bool,
    claims: &mut Claims<'_>,
) -> Result<(), SourceError> {
    let mut templates = BTreeSet::new();
    for (file, root) in files {
        for (_, section) in root.checked_entries(file)? {
            if let Some(names) = section.checked_get(file, TEMPLATE)? {
                templates.extend(names.list().iter().filter_map(Node::text));
            }
        }
    }
    let mut declarations = BTreeMap::new();
    let mut references = Vec::new();
    let rules = keys::item_rules();
    for (file, root) in files {
        for (id, section) in root
            .checked_entries(file)?
            .iter()
            .filter(|(id, n)| n.mapping() && !templates.contains(id.as_str()))
        {
            let location = format!("{file}:{}", section.key_line);
            declarations.insert(id.as_str(), location.clone());
            claims.declaration = Some(location);
            item(id, section, file, prefer, &rules, claims)?;
            claims.declaration = None;
            if let Some(mechanics) = section.checked_get(file, MECHANICS)? {
                let mut found = Vec::new();
                mechanics.checked_find_all(file, NEXO_ITEM, &mut found)?;
                for node in found {
                    references.push((
                        Some(id.as_str()),
                        file.as_str(),
                        node,
                        format!("item {id} Mechanics loot {NEXO_ITEM}"),
                    ));
                }
            }
        }
    }
    for (file, recipe) in recipes {
        let mut found = Vec::new();
        recipe.checked_find_all(file, NEXO_ITEM, &mut found)?;
        for node in found {
            references.push((None, file.as_str(), node, format!("recipe {NEXO_ITEM}")));
        }
    }
    for (from, file, node, reason) in references {
        if let Some(target) = node
            .text()
            .map(str::trim)
            .filter(|s| !s.is_empty() && from != Some(*s))
        {
            let definitions: Vec<_> = declarations
                .get(target)
                .into_iter()
                .flat_map(|declaration| {
                    claims.out.iter().filter(move |u| {
                        u.kind == ItemDefinition
                            && u.declares.as_ref() == Some(declaration)
                            && !u
                                .reason
                                .as_ref()
                                .is_some_and(|r| r.starts_with("unresolvable"))
                    })
                })
                .map(|u| u.id.0.to_string())
                .collect();
            let definitions = if definitions.is_empty() {
                vec![format!("{GENERATED_NAMESPACE}:{}", target.to_lowercase())]
            } else {
                definitions
            };
            for definition in definitions {
                claims.out.push(usage(
                    ItemDefinition,
                    &definition,
                    format!("{file}:{}", node.line),
                    Some(format!("{reason} {target}")),
                ));
            }
        }
    }
    Ok(())
}

fn item(
    id: &str,
    section: &Node,
    file: &str,
    prefer: bool,
    rules: &[keys::Rule],
    claims: &mut Claims<'_>,
) -> Result<(), SourceError> {
    let template = section.checked_get(file, TEMPLATE)?;
    if let Some(names) = template {
        claims.unresolvable(ItemDefinition, &format!("template {}", names.list().iter().filter_map(Node::text).collect::<Vec<_>>().join(", ")), format!("{file}:{}", names.line), format!("item {id} merges template sections and fills their placeholders at build; only the keys it sets itself are read"));
    }
    let pack = section.checked_get(file, PACK)?.filter(|n| n.mapping());
    let built = section
        .checked_get(file, ITEM_MODEL_SECTION)?
        .filter(|n| n.mapping());
    if let Some(built) = built {
        claims.unresolvable(Model, ITEM_MODEL_SECTION, format!("{file}:{}", built.key_line), format!("item {id} builds its item definition in an ItemModel section; the models it names are not read"));
    }
    for rule in rules {
        if let Some(found) = section.checked_get(file, &rule.path)? {
            let reason = format!(
                "item {id} {}{}",
                rule.path,
                if rule.note.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", rule.note)
                }
            );
            let values = if rule.each {
                found
                    .checked_entries(file)?
                    .iter()
                    .map(|(_, v)| v)
                    .collect()
            } else {
                found.list().iter().collect::<Vec<_>>()
            };
            for value in values {
                for kind in &rule.kinds {
                    claims.value(*kind, value, rule.form, file, reason.clone());
                }
            }
        }
    }
    if let Some(pack) = pack {
        if let Some(bbmodel) = section.checked_get(file, PACK_BBMODEL)? {
            claims.unresolvable(Model, bbmodel.text().unwrap_or(PACK_BBMODEL), format!("{file}:{}", bbmodel.line), format!("item {id} Pack.bbmodel is converted to a model at build; the file is not followed"));
        }
        let mut has_texture = false;
        for path in PACK_TEXTURES {
            has_texture |= section.checked_get(file, path)?.is_some();
        }
        if section.checked_get(file, PACK_MODEL)?.is_none()
            && section.checked_get(file, PACK_BBMODEL)?.is_none()
            && !has_texture
        {
            claims.id(
                Model,
                &id.to_lowercase().replace(' ', "_"),
                Form::AsWritten,
                format!("{file}:{}", pack.key_line),
                format!(
                    "item {id} Pack names no model or texture: the model is named after the item"
                ),
            );
        }
        let material = section.checked_get(file, MATERIAL)?.and_then(Node::text);
        if let Some(cmd) = section
            .checked_get(file, CUSTOM_MODEL_DATA)?
            .filter(|_| material.is_some() || template.is_none())
        {
            let written = material
                .map(|s| s.trim().to_lowercase())
                .unwrap_or(DEFAULT_MATERIAL.into());
            let name = ResourceId::parse(&claims.cx.interner, &written)
                .filter(|id| claims.cx.interner.resolve(id.ns) == "minecraft")
                .map(|id| id.display(&claims.cx.interner).to_string())
                .unwrap_or(format!("minecraft:{DEFAULT_MATERIAL}"));
            claims.id(
                ItemDefinition,
                &name,
                Form::AsWritten,
                format!("{file}:{}", cmd.line),
                format!("item {id} custom_model_data: a case of the definition of its material"),
            );
        }
    }
    if (pack.is_some() || built.is_some())
        && template.is_none()
        && section.checked_get(file, ITEM_MODEL_COMPONENT)?.is_none()
        && (prefer || built.is_some())
    {
        claims.id(
            ItemDefinition,
            &format!("{GENERATED_NAMESPACE}:{}", id.to_lowercase()),
            Form::AsWritten,
            format!("{file}:{}", section.key_line),
            format!("item {id}: the definition Nexo generates for it"),
        );
    }
    if let Some(armor) = section.checked_get(file, ARMOR)?.filter(|n| n.mapping()) {
        if let Some(armor_id) = armor.checked_get(file, ARMOR_ID)?
            && let Some(text) = armor_id.text().filter(|s| !s.trim().is_empty())
        {
            claims.id(
                Equipment,
                &format!("{GENERATED_NAMESPACE}:{}", text.trim().to_lowercase()),
                Form::AsWritten,
                format!("{file}:{}", armor_id.line),
                format!("item {id} CustomArmor.id: the equipment asset Nexo generates"),
            );
        }
        for layer in ARMOR_LAYERS {
            if let Some(texture) = armor.checked_get(file, layer)? {
                claims.value(
                    Sprite,
                    texture,
                    Form::WithoutPng,
                    file,
                    format!("item {id} {ARMOR}.{layer}"),
                );
            }
        }
    }
    Ok(())
}
