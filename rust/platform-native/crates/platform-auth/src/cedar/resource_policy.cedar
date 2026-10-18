permit(principal is Objects::Subject, action, resource is Objects::Tenant)
when { resource.present &&
 (principal.normalized_roles.containsAny(["platform_admin","superadmin","super_admin"]) ||
  (principal.tenant != "" && principal.tenant == resource.tenant &&
   (principal.roles.contains("admin") || principal.tier_lower == "admin"))) };

permit(principal is Objects::Subject, action, resource is Objects::User)
when { resource.present && principal.user_trim != "" && principal.tenant_trim != "" && principal.tenant == resource.tenant &&
 (resource.profile == "user_actor" && principal.user == resource.id &&
  (context.operation == "profile" || resource.current_binding && principal.roles.containsAny(["admin","platform_admin"])) ||
  resource.profile == "user_mutation" &&
  (context.operation == "profile" && principal.user == resource.id || context.operation != "profile" &&
   principal.user != resource.id && !resource.protected && principal.roles.containsAny(["admin","platform_admin"])) ||
  resource.profile == "standard" &&
  (context.operation == "profile" && principal.user == resource.id ||
   context.operation != "profile" && principal.permissions.containsAny(context.permission_candidates) &&
   (context.operation == "read" || principal.roles.containsAny(["admin","platform_admin"])))) };

permit(principal is Objects::Subject, action, resource is Objects::Dataset)
when { resource.present && !resource.archived && resource.identity_current &&
 (principal.anonymous && resource.visibility == "public" && context.rank <= 1 ||
  !principal.anonymous &&
  ((principal.user != "" && principal.user == resource.owner) ||
   (principal.tenant_trim != "" && principal.tenant_trim == resource.tenant_trim &&
    (principal.tier == "admin" || principal.roles.contains("admin"))) ||
   resource.grants.contains("owner") ||
   (context.rank <= 2 && resource.grants.contains("editor")) ||
   (context.rank <= 1 && resource.grants.contains("viewer")) ||
   (!(resource.nonempty_grant_count > 0 && resource.profile != "dataset_query" || resource.profile == "dataset_query" && resource.grants.containsAny(["viewer","editor","owner"])) && context.rank <= 1 &&
    (resource.visibility == "public" || resource.visibility == "tenant" &&
     principal.tenant_trim != "" && principal.tenant_trim == resource.tenant_trim)))) };

permit(principal is Objects::Subject, action, resource is Objects::Agent)
when {resource.present && principal.tenant == resource.tenant &&
 (resource.profile == "agent_creation" && resource.current_binding && principal.user == resource.owner ||
  resource.profile != "agent_creation" &&
  (principal.tier_lower == "admin" || principal.folded_roles.containsAny(["admin","tenant_admin"]) ||
   resource.profile == "agent_listing" && resource.current_binding ||
   resource.grants.contains("owner") || context.rank <= 2 && resource.grants.contains("editor") ||
   context.rank <= 1 && resource.grants.contains("viewer")))};

permit(principal is Objects::Subject, action, resource is Objects::Tool)
when {resource.present && principal.tenant == resource.tenant &&
 (resource.profile == "tool_visibility" && resource.alias_count > 0 && resource.satisfied_scopes.containsAll(resource.required_scopes) &&
  (principal.roles.contains("admin") ||
   !resource.denied_aliases.containsAny(resource.aliases) &&
   (!resource.allowlist_mode || resource.allowed_aliases.containsAny(resource.aliases))) ||
  resource.profile == "standard" && !principal.anonymous && principal.user != "" &&
  principal.tenant != "" && principal.tenant != "public" &&
  principal.capability_permissions.containsAny(context.permission_candidates) &&
  (context.operation == "read" || principal.folded_roles.containsAny(["admin","tenant_admin","superadmin","super_admin"])))};

permit(principal is Objects::Subject, action, resource is Objects::Model)
when {resource.present && resource.enabled && principal.tenant == resource.tenant &&
 (resource.profile == "model_administration" &&
  principal.model_admin_permissions.containsAny(context.permission_candidates) ||
  resource.profile == "standard" &&
  (resource.access_level == "public" || resource.access_level == "premium" &&
   (principal.tier_lower == "admin" || principal.folded_roles.containsAny(["admin","tenant_admin"]) ||
    principal.tier_lower == "premium" || principal.tier_lower == "enterprise") ||
   resource.access_level == "admin" &&
   (principal.tier_lower == "admin" || principal.folded_roles.containsAny(["admin","tenant_admin"]))))};
