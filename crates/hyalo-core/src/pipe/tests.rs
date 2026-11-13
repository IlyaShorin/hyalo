use super::*;

const POSITION: Position = Position {
    volume: 0xFEDC_BA98_7654_3210,
    journal: 42,
    next: 1 << 40,
};

/// The body of a frame: what follows its length, which must say it.
fn body(frame: &[u8]) -> &[u8] {
    let head: [u8; 4] = frame[..4].try_into().unwrap();
    assert_eq!(length(head), Some(frame.len() - 4), "the length says it");
    &frame[4..]
}

fn files() -> Vec<Change> {
    vec![
        Change {
            record: 40,
            file: Some(File {
                folder: true,
                size: 0,
                written: 1_794_565_800,
                names: vec![Name {
                    parent: 5,
                    text: "Документы".to_string(),
                }],
            }),
        },
        Change {
            record: 70,
            file: Some(File {
                folder: false,
                size: u64::MAX,
                written: -11_644_473_600,
                names: vec![
                    Name {
                        parent: 40,
                        text: "a.txt".to_string(),
                    },
                    Name {
                        parent: 5,
                        text: "😀 b.txt".to_string(),
                    },
                ],
            }),
        },
        Change {
            record: u32::MAX - 1,
            file: None,
        },
        Change {
            record: 71,
            file: Some(File::default()),
        },
    ]
}

#[test]
fn every_request_reads_back_the_same() {
    for request in [
        Request::Follow {
            drive: 'C',
            from: None,
        },
        Request::Follow {
            drive: 'Z',
            from: Some(POSITION),
        },
        Request::Subtree {
            drive: 'D',
            records: vec![40, 41, u32::MAX],
        },
        Request::Subtree {
            drive: 'A',
            records: vec![],
        },
    ] {
        assert_eq!(Request::read(body(&request.bytes())), Ok(request));
    }
}

#[test]
fn every_answer_reads_back_the_same() {
    for answer in [
        Answer::Outdated(7),
        Answer::Refused,
        Answer::Listing(POSITION),
        Answer::Files(files()),
        Answer::Files(vec![]),
        Answer::Listed,
        Answer::Changed(POSITION, files()),
    ] {
        assert_eq!(Answer::read(body(&answer.bytes())), Some(answer));
    }
}

#[test]
fn a_request_of_another_version_is_told_apart_from_garble() {
    let mut other = Request::Follow {
        drive: 'C',
        from: None,
    }
    .bytes();
    other[4..8].copy_from_slice(&(VERSION + 1).to_le_bytes());
    assert_eq!(Request::read(&other[4..]), Err(Unread::Outdated));
    assert_eq!(Request::read(&[1, 2]), Err(Unread::Garbled));
    assert_eq!(Request::read(&[]), Err(Unread::Garbled));
}

#[test]
fn the_answer_of_another_version_never_changes() {
    assert_eq!(Answer::Outdated(3).bytes(), [5, 0, 0, 0, 0, 3, 0, 0, 0]);
}

#[test]
fn a_drive_is_a_letter() {
    let mut frame = Request::Follow {
        drive: 'C',
        from: None,
    }
    .bytes();
    let drive = 4 + 4 + 1;
    for refused in [b'c', b'@', b'[', b'1', 0, 0xC0] {
        frame[drive] = refused;
        assert_eq!(
            Request::read(&frame[4..]),
            Err(Unread::Garbled),
            "{refused}"
        );
    }
}

#[test]
fn an_answer_cut_short_anywhere_or_with_more_after_is_refused() {
    let answer = Answer::Changed(POSITION, files()).bytes();
    for end in 4..answer.len() {
        assert_eq!(Answer::read(&answer[4..end]), None, "cut at {end}");
    }
    let longer = [&answer[4..], &[0]].concat();
    assert_eq!(Answer::read(&longer), None);
}

#[test]
fn a_request_cut_short_anywhere_or_with_more_after_is_refused() {
    let request = Request::Subtree {
        drive: 'C',
        records: vec![1, 2],
    }
    .bytes();
    for end in 4..request.len() {
        assert_eq!(Request::read(&request[4..end]), Err(Unread::Garbled));
    }
    let longer = [&request[4..], &[0]].concat();
    assert_eq!(Request::read(&longer), Err(Unread::Garbled));
}

#[test]
fn a_name_that_is_not_utf8_or_a_kind_not_known_is_refused() {
    let answer = Answer::Files(files()).bytes();
    let at = answer
        .windows(5)
        .position(|bytes| bytes == b"a.txt")
        .expect("the name");
    let mut broken = answer.clone();
    broken[at] = 0xFF;
    assert_eq!(Answer::read(&broken[4..]), None);
    let mut unknown = answer.clone();
    unknown[4] = 99;
    assert_eq!(Answer::read(&unknown[4..]), None);
    let gone = answer
        .windows(5)
        .position(|bytes| bytes == [0xFE, 0xFF, 0xFF, 0xFF, 0])
        .expect("the record gone");
    let mut state = answer.clone();
    state[gone + 4] = 3;
    assert_eq!(
        Answer::read(&state[4..]),
        None,
        "neither gone, file nor folder"
    );
}

#[test]
fn a_frame_longer_than_either_side_reads_is_refused() {
    assert_eq!(length((MOST as u32).to_le_bytes()), Some(MOST));
    assert_eq!(length((MOST as u32 + 1).to_le_bytes()), None);
    assert_eq!(length([0; 4]), Some(0));
}

#[test]
fn a_request_of_a_kind_not_known_is_refused() {
    let mut frame = Request::Follow {
        drive: 'C',
        from: None,
    }
    .bytes();
    frame[8] = 7;
    assert_eq!(Request::read(&frame[4..]), Err(Unread::Garbled));
    let mut from = Request::Follow {
        drive: 'C',
        from: None,
    }
    .bytes();
    from[10] = 2;
    assert_eq!(
        Request::read(&from[4..]),
        Err(Unread::Garbled),
        "neither from nothing nor from a position"
    );
}
