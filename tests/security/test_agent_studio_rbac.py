from __future__ import annotations

from src.api.deps import get_user_context
from tests.api.test_agents_api import (
    InMemoryAgentRepository,
    create_agent,
    make_client,
    make_user,
    valid_spec,
)


def _client_as(client, user) -> None:
    client.app.dependency_overrides[get_user_context] = lambda: user


def test_viewer_is_read_only_and_cannot_create_versions_or_copy() -> None:
    client, repository = make_client()
    agent = create_agent(client)
    agent_id = agent["agent_id"]
    record = repository.records[("tenant-a", agent_id)]
    record["members"]["viewer-a"] = "viewer"
    record["member_created_by"]["viewer-a"] = "owner-a"
    _client_as(client, make_user("viewer-a", "tenant-a"))

    assert client.get(f"/agents/{agent_id}").status_code == 200
    assert client.get(f"/agents/{agent_id}/draft").status_code == 200
    assert client.get(f"/agents/{agent_id}/versions").status_code == 200

    denied = [
        client.patch(f"/agents/{agent_id}", json={"name": "Denied"}),
        client.put(
            f"/agents/{agent_id}/draft",
            headers={"If-Match": '"1"'},
            json={"spec": valid_spec("Denied")},
        ),
        client.post(f"/agents/{agent_id}/versions", headers={"If-Match": '"1"'}),
        client.post(f"/agents/{agent_id}/copy", json={}),
        client.post(f"/agents/{agent_id}/archive", json={}),
        client.put(
            f"/agents/{agent_id}/members/user/another-user",
            json={"role": "editor"},
        ),
        client.delete(f"/agents/{agent_id}"),
    ]
    assert {response.status_code for response in denied} == {404}
    assert all(response.json()["detail"]["code"] == "AGENT_NOT_FOUND" for response in denied)


def test_editor_can_edit_metadata_and_draft_but_not_owner_operations() -> None:
    client, repository = make_client()
    agent = create_agent(client)
    agent_id = agent["agent_id"]
    record = repository.records[("tenant-a", agent_id)]
    record["members"]["editor-a"] = "editor"
    record["member_created_by"]["editor-a"] = "owner-a"
    _client_as(client, make_user("editor-a", "tenant-a"))

    metadata = client.patch(f"/agents/{agent_id}", json={"description": "Edited"})
    assert metadata.status_code == 200
    draft = client.put(
        f"/agents/{agent_id}/draft",
        headers={"If-Match": '"1"'},
        json={"spec": valid_spec("Editor instruction")},
    )
    assert draft.status_code == 200

    assert client.post(f"/agents/{agent_id}/versions", headers={"If-Match": '"2"'}).status_code == 404
    assert client.post(f"/agents/{agent_id}/copy", json={}).status_code == 404
    assert client.post(f"/agents/{agent_id}/archive", json={}).status_code == 404
    assert (
        client.put(
            f"/agents/{agent_id}/members/user/viewer-b",
            json={"role": "viewer"},
        ).status_code
        == 404
    )


def test_owner_acl_and_last_owner_protection() -> None:
    client, repository = make_client()
    agent = create_agent(client)
    agent_id = agent["agent_id"]

    demote_last = client.put(
        f"/agents/{agent_id}/members/user/owner-a",
        json={"role": "editor"},
    )
    assert demote_last.status_code == 409
    assert demote_last.json()["detail"]["code"] == "AGENT_LAST_OWNER"

    remove_last = client.delete(f"/agents/{agent_id}/members/user/owner-a")
    assert remove_last.status_code == 409
    assert remove_last.json()["detail"]["code"] == "AGENT_LAST_OWNER"

    add_owner = client.put(
        f"/agents/{agent_id}/members/user/owner-b",
        json={"role": "owner"},
    )
    assert add_owner.status_code == 200
    remove_first = client.delete(f"/agents/{agent_id}/members/user/owner-a")
    assert remove_first.status_code == 200
    assert repository.records[("tenant-a", agent_id)]["members"] == {"owner-b": "owner"}
    assert repository.records[("tenant-a", agent_id)]["agent"]["owner_id"] == "owner-b"


def test_cross_tenant_object_paths_return_not_found_and_lists_do_not_leak() -> None:
    repository = InMemoryAgentRepository()
    owner_client, _ = make_client(repository, make_user("owner-a", "tenant-a"))
    agent = create_agent(owner_client)
    agent_id = agent["agent_id"]

    tenant_b_client, _ = make_client(repository, make_user("owner-b", "tenant-b"))
    assert tenant_b_client.get("/agents").json() == {"items": [], "next_cursor": None}

    responses = [
        tenant_b_client.get(f"/agents/{agent_id}"),
        tenant_b_client.get(f"/agents/{agent_id}/draft"),
        tenant_b_client.patch(f"/agents/{agent_id}", json={"name": "Cross tenant"}),
        tenant_b_client.put(
            f"/agents/{agent_id}/draft",
            headers={"If-Match": '"1"'},
            json={"spec": valid_spec("Cross tenant")},
        ),
        tenant_b_client.post(f"/agents/{agent_id}/versions", headers={"If-Match": '"1"'}),
        tenant_b_client.post(f"/agents/{agent_id}/copy", json={}),
        tenant_b_client.delete(f"/agents/{agent_id}"),
    ]
    assert {response.status_code for response in responses} == {404}
    assert all(response.json()["detail"]["code"] == "AGENT_NOT_FOUND" for response in responses)
    assert all("tenant-a" not in response.text for response in responses)


def test_tenant_admin_can_manage_only_their_tenant() -> None:
    repository = InMemoryAgentRepository()
    owner_client, _ = make_client(repository, make_user("owner-a", "tenant-a"))
    agent = create_agent(owner_client)

    admin_client, _ = make_client(repository, make_user("admin-a", "tenant-a", "admin"))
    assert admin_client.get(f"/agents/{agent['agent_id']}").status_code == 200
    assert (
        admin_client.patch(f"/agents/{agent['agent_id']}", json={"description": "Admin edit"}).status_code
        == 200
    )

    other_admin, _ = make_client(repository, make_user("admin-b", "tenant-b", "admin"))
    assert other_admin.get(f"/agents/{agent['agent_id']}").status_code == 404
    assert other_admin.get("/agents").json()["items"] == []


def test_unauthenticated_and_public_contexts_are_rejected() -> None:
    anonymous_client, _ = make_client(
        user=make_user("guest", "public", authenticated=False)
    )
    unauthenticated = anonymous_client.get("/agents")
    assert unauthenticated.status_code == 401
    assert unauthenticated.json()["detail"]["code"] == "AUTHENTICATION_REQUIRED"

    public_client, _ = make_client(user=make_user("api-client", "public"))
    public = public_client.post("/agents", json={"name": "Public denied"})
    assert public.status_code == 403
    assert public.json()["detail"]["code"] == "TENANT_REQUIRED"
