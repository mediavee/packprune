use packprune_pack::json::{Member, Node, Value};

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct NoOptions {}

pub(crate) fn set(node: &mut Node, name: &str, value: Node) {
    let Value::Object(members) = &mut node.value else {
        unreachable!("the parsed container is an object")
    };
    if let Some(member) = members.iter_mut().find(|member| &*member.name == name) {
        member.node = value;
    } else {
        members.push(Member::new(name, value));
    }
}

pub(crate) fn object(fields: &[(&str, Node)]) -> Node {
    Node::new(Value::Object(
        fields
            .iter()
            .map(|(key, value)| Member::new(key, value.clone()))
            .collect(),
    ))
}

pub(crate) fn string(value: &str) -> Node {
    Node::new(Value::String(value.into()))
}
