# Learnings: RTM_NEWLINK Implementation

## Module Structure
- Converted flat `route.rs` into `route/` directory module with `mod.rs`, `link.rs`, `addr.rs`, `route.rs`
- NetlinkSocket is defined in `netlink/mod.rs` → from submodules use `super::super::NetlinkSocket`

## IFLA_LINKINFO Nested Parsing (4 levels)
- Top: IFLA_LINKINFO (rta_type & !NLA_F_NESTED) → contains IFLA_INFO_*
- L1: IFLA_INFO_KIND (string) / IFLA_INFO_DATA (nested)
- L2: IFLA_INFO_DATA → VETH_INFO_PEER (nested)
- L3: VETH_INFO_PEER → ifinfomsg(16B) + IFLA_IFNAME for peer name

## NLA_F_NESTED Convention
- rta_type_raw & NLA_F_NESTED ≠ 0 means attribute contains nested sub-attributes
- Match uses stripped type: rta_type = rta_type_raw & !NLA_F_NESTED

## NLM_F Flag Semantics (Linux-compatible)
- NLM_F_EXCL: fail with EEXIST(17) if name already taken
- NLM_F_CREATE: create if not exists (default when neither set)
- Effective: create || !excl (defaults to NLM_F_CREATE behavior)

## Error Codes
- Missing IFLA_INFO_KIND or IFLA_IFNAME → EINVAL(22)
- Unsupported link kind (e.g. "bridge") → EOPNOTSUPP(95)
- Duplicate name with NLM_F_EXCL → EEXIST(17)
