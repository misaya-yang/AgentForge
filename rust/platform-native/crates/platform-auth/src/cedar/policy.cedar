permit (
    principal is AgentForge::Subject,
    action,
    resource is AgentForge::Resource
)
when {
    principal.tenant == resource.tenant &&
    principal.permissions.containsAny(context.accepted)
};
