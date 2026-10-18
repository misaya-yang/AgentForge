use super::*;

#[tokio::test]
async fn exited_leader_still_cleans_and_reaps_term_resistant_grandchild() {
    prepare_descendant_cleanup().unwrap();
    let mut child = Command::new("/bin/sh")
        .arg("-c")
        .arg("trap '' TERM; sleep 60 & printf '%s\\n' \"$!\"; exit 0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let group = child.id().unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let mut pid = String::new();
    reader.read_line(&mut pid).await.unwrap();
    let grandchild: i32 = pid.trim().parse().unwrap();
    assert!(child.wait().await.unwrap().success());
    assert!(signal_group(group, 0).unwrap());
    cleanup(&mut child, group).await.unwrap();
    assert!(!signal_group(group, 0).unwrap());
    assert_eq!(unsafe { libc::kill(grandchild, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
}

#[tokio::test]
async fn cleanup_of_reaped_child_is_idempotent() {
    prepare_descendant_cleanup().unwrap();
    let mut child = Command::new("/bin/sh")
        .args(["-c", "exit 0"])
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let group = child.id().unwrap();
    assert!(child.wait().await.unwrap().success());
    cleanup(&mut child, group).await.unwrap();
    cleanup(&mut child, group).await.unwrap();
    assert!(!signal_group(group, 0).unwrap());
}
