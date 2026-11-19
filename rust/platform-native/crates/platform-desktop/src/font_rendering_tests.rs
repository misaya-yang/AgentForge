//! Real font rasterization and background session fencing for the maintained egui backend.
use super::*;
#[test]
fn hidden_window_message_processing_fences_logout_without_a_draw_or_network_request() {
    let mut app = App::default();
    app.client = Some(
        Client::new("https://gateway.example")
            .unwrap()
            .authenticated("synthetic-token".into()),
    );
    app.session = "synthetic-session".into();
    app.password = "synthetic-password".into();
    app.sender.send(ResultMessage::Logout(0, Ok(()))).unwrap();
    app.process_messages(&egui::Context::default());
    assert!(app.client.is_none());
    assert!(app.session.is_empty());
    assert!(app.password.is_empty());
    assert_eq!(app.status, "Signed out");
    assert_eq!(app.generation, 1);
}

#[test]
fn real_desktop_sign_in_renders_default_fonts_and_keeps_requests_user_owned() {
    let mut app = App::default();
    let context = egui::Context::default();
    let output = context.run_ui(egui::RawInput::default(), |ui| app.render_ui(ui));
    assert!(
        !output.textures_delta.set.is_empty(),
        "actual font atlas must be rasterized"
    );
    let meshes = context.tessellate(output.shapes, output.pixels_per_point);
    assert!(meshes.iter().any(|item| matches!(&item.primitive, egui::epaint::Primitive::Mesh(mesh) if !mesh.vertices.is_empty())), "actual desktop text and widgets must tessellate");
    assert!(app.client.is_none());
    assert!(!app.busy);
    assert_eq!(app.generation, 0);
    assert!(app.password.is_empty());
    assert!(app.receiver.try_recv().is_err());
}
