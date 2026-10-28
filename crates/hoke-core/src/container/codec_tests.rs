use super::*;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn reference(kind: SegmentKind, offset: u64) -> SegmentRef {
    SegmentRef {
        kind,
        offset,
        payload_len: 3,
    }
}

fn sample_root() -> Root {
    Root {
        generation: 7,
        tables: vec![reference(SegmentKind::Sst, 32)],
        wal: vec![reference(SegmentKind::Wal, 71)],
    }
}

#[test]
fn file_and_record_headers_match_independent_golden_bytes() {
    // CRC fixtures computed independently with Python zlib.crc32.
    let file = encode_file_header();
    assert_eq!(
        hex(&file),
        "484f4b45434e540001000000200000000000000000000000000000007fc4d00f"
    );
    assert_eq!(decode_file_header(&file), Ok(()));
    let header = RecordHeader {
        kind: RecordKind::Wal,
        sequence: 1,
        payload_len: 3,
    };
    let record = encode_record_header(&header).unwrap();
    assert_eq!(
        hex(&record),
        "484b5231010000000100000000000000030000000000000000000000048ab996"
    );
    assert_eq!(decode_record_header(&record), Ok(header));
}

#[test]
fn every_header_byte_is_protected_and_lengths_are_exact() {
    let file = encode_file_header();
    let record = encode_record_header(&RecordHeader {
        kind: RecordKind::Sst,
        sequence: 2,
        payload_len: 0,
    })
    .unwrap();
    for index in 0..32 {
        let mut bad = file;
        bad[index] ^= 1;
        assert_eq!(
            decode_file_header(&bad),
            Err(CodecError::Invalid("header checksum"))
        );
        let mut bad = record;
        bad[index] ^= 1;
        assert_eq!(
            decode_record_header(&bad),
            Err(CodecError::Invalid("header checksum"))
        );
    }
    for len in 0..32 {
        assert!(decode_file_header(&file[..len]).is_err());
        assert!(decode_record_header(&record[..len]).is_err());
    }
    assert!(decode_file_header(&[0; 33]).is_err());
    assert!(decode_record_header(&[0; 33]).is_err());
}

#[test]
fn repaired_checksums_do_not_bypass_file_semantics() {
    for offset in [0, 12, 16, 27] {
        let mut bytes = encode_file_header();
        bytes[offset] ^= 1;
        finish_header(&mut bytes);
        assert!(decode_file_header(&bytes).is_err());
    }
    let mut bytes = encode_file_header();
    bytes[8..12].copy_from_slice(&2_u32.to_le_bytes());
    assert_eq!(
        decode_file_header(&bytes),
        Err(CodecError::Invalid("header checksum"))
    );
    finish_header(&mut bytes);
    assert_eq!(
        decode_file_header(&bytes),
        Err(CodecError::UnsupportedVersion(2))
    );
}

#[test]
fn record_caps_kinds_sequences_and_reserved_bytes_are_validated() {
    let original = RecordHeader {
        kind: RecordKind::Wal,
        sequence: 1,
        payload_len: 0,
    };
    for offset in [0, 4, 5, 7, 24, 27] {
        let mut bytes = encode_record_header(&original).unwrap();
        bytes[offset] = 255;
        finish_header(&mut bytes);
        assert!(decode_record_header(&bytes).is_err());
    }
    for header in [
        RecordHeader {
            sequence: 0,
            ..original
        },
        RecordHeader {
            payload_len: MAX_SEGMENT_BYTES + 1,
            ..original
        },
        RecordHeader {
            kind: RecordKind::Root,
            payload_len: 0,
            ..original
        },
        RecordHeader {
            kind: RecordKind::Root,
            payload_len: 25,
            ..original
        },
        RecordHeader {
            kind: RecordKind::Root,
            payload_len: MAX_ROOT_BYTES as u64 + 24,
            ..original
        },
    ] {
        assert!(encode_record_header(&header).is_err());
        let mut bytes = encode_record_header(&original).unwrap();
        bytes[4] = header.kind as u8;
        bytes[8..16].copy_from_slice(&header.sequence.to_le_bytes());
        bytes[16..24].copy_from_slice(&header.payload_len.to_le_bytes());
        finish_header(&mut bytes);
        assert!(decode_record_header(&bytes).is_err());
    }
    for (kind, payload_len) in [
        (RecordKind::Wal, 0),
        (RecordKind::Sst, MAX_SEGMENT_BYTES),
        (RecordKind::Root, 24),
        (RecordKind::Root, MAX_ROOT_BYTES as u64),
    ] {
        let header = RecordHeader {
            kind,
            sequence: u64::MAX,
            payload_len,
        };
        assert_eq!(
            decode_record_header(&encode_record_header(&header).unwrap()),
            Ok(header)
        );
    }
}

#[test]
fn ref_golden_and_safe_end_offset_boundaries() {
    let item = reference(SegmentKind::Sst, 32);
    assert_eq!(
        hex(&encode_ref(&item).unwrap()),
        "020000000000000020000000000000000300000000000000"
    );
    assert_eq!(decode_ref(&encode_ref(&item).unwrap()), Ok(item));
    let last = SegmentRef {
        kind: SegmentKind::Wal,
        offset: MAX_FILE_BYTES - 36,
        payload_len: 0,
    };
    assert_eq!(decode_ref(&encode_ref(&last).unwrap()), Ok(last));
    for bad in [
        SegmentRef { offset: 31, ..item },
        SegmentRef {
            offset: MAX_FILE_BYTES - 35,
            payload_len: 0,
            ..item
        },
        SegmentRef {
            offset: u64::MAX,
            ..item
        },
        SegmentRef {
            payload_len: MAX_SEGMENT_BYTES + 1,
            ..item
        },
    ] {
        assert!(encode_ref(&bad).is_err());
        let mut bytes = encode_ref(&item).unwrap();
        bytes[8..16].copy_from_slice(&bad.offset.to_le_bytes());
        bytes[16..24].copy_from_slice(&bad.payload_len.to_le_bytes());
        assert!(decode_ref(&bytes).is_err());
    }
}

#[test]
fn ref_rejects_root_unknown_kinds_reserved_bytes_and_wrong_size() {
    let valid = encode_ref(&reference(SegmentKind::Wal, 32)).unwrap();
    for offset in 0..8 {
        let mut bad = valid;
        bad[offset] = 3;
        assert!(decode_ref(&bad).is_err());
    }
    for len in 0..24 {
        assert!(decode_ref(&valid[..len]).is_err());
    }
    assert!(decode_ref(&[0; 25]).is_err());
    for kind in [RecordKind::Wal, RecordKind::Sst] {
        assert_eq!(kind.segment_kind().unwrap().record_kind(), kind);
    }
    assert_eq!(RecordKind::Root.segment_kind(), None);
    assert!(RecordKind::try_from(4).is_err());
}

#[test]
fn root_matches_golden_preserves_list_order_and_accepts_empty() {
    let root = sample_root();
    let bytes = encode_root(&root).unwrap();
    assert_eq!(
        hex(&bytes),
        "070000000000000001000000010000000000000000000000020000000000000020000000000000000300000000000000010000000000000047000000000000000300000000000000"
    );
    assert_eq!(decode_root(&bytes), Ok(root));
    let empty = Root {
        generation: 1,
        tables: vec![],
        wal: vec![],
    };
    assert_eq!(encode_root(&empty).unwrap().len(), 24);
    assert_eq!(decode_root(&encode_root(&empty).unwrap()), Ok(empty));
    let mut ordered = sample_root();
    ordered.tables.insert(0, reference(SegmentKind::Sst, 500));
    assert_eq!(decode_root(&encode_root(&ordered).unwrap()), Ok(ordered));
}

#[test]
fn root_rejects_malformed_counts_generations_and_reserved_prefix() {
    let valid = encode_root(&sample_root()).unwrap();
    for len in 0..valid.len() {
        assert!(decode_root(&valid[..len]).is_err());
    }
    let mut bad = valid.clone();
    bad.push(0);
    assert!(decode_root(&bad).is_err());
    for offset in [8, 12] {
        let mut bad = valid.clone();
        bad[offset..offset + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(decode_root(&bad).is_err());
    }
    for offset in 16..24 {
        let mut bad = valid.clone();
        bad[offset] = 1;
        assert!(decode_root(&bad).is_err());
    }
    let mut bad = valid;
    bad[..8].fill(0);
    assert!(decode_root(&bad).is_err());
    assert!(
        encode_root(&Root {
            generation: 0,
            ..sample_root()
        })
        .is_err()
    );
}

#[test]
fn root_rejects_duplicate_offsets_even_across_lists_and_wrong_kinds() {
    let original = sample_root();
    let mut cross_list = original.clone();
    cross_list.wal[0].offset = cross_list.tables[0].offset;
    assert!(encode_root(&cross_list).is_err());
    for duplicate_tables in [true, false] {
        let mut root = original.clone();
        if duplicate_tables {
            root.tables.push(root.tables[0]);
        } else {
            root.wal.push(root.wal[0]);
        }
        assert!(encode_root(&root).is_err());
        // Bypass the encoder to check malformed bytes on the read path too.
        let mut bytes = encode_root(&original).unwrap();
        let (count_offset, reference_offset) = if duplicate_tables {
            (8, ROOT_PREFIX_LEN)
        } else {
            (12, ROOT_PREFIX_LEN + REF_LEN)
        };
        bytes[count_offset..count_offset + 4].copy_from_slice(&2_u32.to_le_bytes());
        let duplicate = bytes[reference_offset..reference_offset + REF_LEN].to_vec();
        bytes.splice(reference_offset..reference_offset, duplicate);
        assert!(decode_root(&bytes).is_err());
    }
    let mut encoded = encode_root(&original).unwrap();
    encoded[ROOT_PREFIX_LEN + REF_LEN + 8..ROOT_PREFIX_LEN + REF_LEN + 16]
        .copy_from_slice(&32_u64.to_le_bytes());
    assert!(decode_root(&encoded).is_err());
    for offset in [ROOT_PREFIX_LEN, ROOT_PREFIX_LEN + REF_LEN] {
        let mut encoded = encode_root(&original).unwrap();
        encoded[offset] = if offset == ROOT_PREFIX_LEN { 1 } else { 2 };
        assert!(decode_root(&encoded).is_err());
    }
    let mut wrong = original.clone();
    wrong.tables[0].kind = SegmentKind::Wal;
    assert!(encode_root(&wrong).is_err());
    let mut wrong = original;
    wrong.wal[0].kind = SegmentKind::Sst;
    assert!(encode_root(&wrong).is_err());
}

#[test]
fn maximum_root_is_bounded_and_one_extra_ref_is_rejected() {
    let mut root = Root {
        generation: u64::MAX,
        tables: vec![],
        wal: (0..MAX_ROOT_REFS)
            .map(|index| reference(SegmentKind::Wal, 32 + index as u64 * 39))
            .collect(),
    };
    let bytes = encode_root(&root).unwrap();
    assert_eq!(bytes.len(), MAX_ROOT_BYTES);
    assert_eq!(decode_root(&bytes).unwrap(), root);
    root.wal.push(reference(SegmentKind::Wal, 1_000_000));
    assert!(encode_root(&root).is_err());
    let mut over = bytes;
    over.extend_from_slice(&[0; REF_LEN]);
    assert!(decode_root(&over).is_err());
}
