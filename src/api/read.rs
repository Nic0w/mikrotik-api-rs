use std::{borrow::Cow, io::Cursor};

use bytes::Buf;

use super::error::Error;

fn get_byte(cursor: &mut Cursor<&[u8]>) -> Option<u8> {
    cursor.has_remaining().then(|| cursor.get_u8())
}

pub(super) fn read_len(cursor: &mut Cursor<&[u8]>) -> Result<u32, Error> {
    let mut next_byte = || get_byte(cursor).ok_or(Error::Incomplete);

    let first_byte = next_byte()?;

    if first_byte >> 7 == 0b0 {
        return Ok(first_byte as u32);
    }

    let mut data: [u8; 4] = [0; 4];

    if first_byte >> 6 == 0b10 {
        data[2] = first_byte & !0xC0;
        data[3] = next_byte()?;

        return Ok(u32::from_be_bytes(data));
    }

    if first_byte >> 5 == 0b110 {
        data[1] = first_byte & !0xE0;
        data[2] = next_byte()?;
        data[3] = next_byte()?;

        return Ok(u32::from_be_bytes(data));
    }

    if first_byte >> 4 == 0b1110 {
        data[0] = first_byte & !0xF0;
        data[1] = next_byte()?;
        data[2] = next_byte()?;
        data[3] = next_byte()?;

        return Ok(u32::from_be_bytes(data));
    }

    if first_byte == 0xF0 {
        data[0] = next_byte()?;
        data[1] = next_byte()?;
        data[2] = next_byte()?;
        data[3] = next_byte()?;

        return Ok(u32::from_be_bytes(data));
    }

    Err(Error::InvalidWordLength(first_byte))
}

fn read_bytes<'buf>(cursor: &mut Cursor<&'buf [u8]>, len: u32) -> Result<&'buf [u8], Error> {
    let start = cursor.position() as usize;
    let end = cursor.get_ref().len();

    let remaining = end - start;

    if len > (remaining as u32) {
        return Err(Error::Incomplete);
    }

    cursor.set_position((start + len as usize) as u64);

    Ok(&cursor.get_ref()[start..start + (len as usize)])
}

fn read_word<'buf>(cursor: &mut Cursor<&'buf [u8]>) -> Result<Cow<'buf, str>, Error> {
    let str_len = read_len(cursor)?;

    let str_bytes = read_bytes(cursor, str_len)?;

    let text = String::from_utf8_lossy(str_bytes);

    Ok(text)
}

pub fn read_sentence<'buf>(cursor: &mut Cursor<&'buf [u8]>) -> Result<Vec<Cow<'buf, str>>, Error> {
    let mut sentence = vec![];

    loop {
        let word = read_word(cursor)?;
        let is_empty = word.is_empty();
        sentence.push(word);

        if is_empty {
            break;
        }
    }

    Ok(sentence)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::read_len;

    #[test]
    fn test_readlen_one_byte() {
        let test_value = 0x42u8;

        let byte = [test_value];

        let mut cursor = Cursor::new(&byte[..]);

        let result = read_len(&mut cursor);

        assert!(result.is_ok());

        let value = result.unwrap();

        assert_eq!(test_value as u32, value);
    }

    #[test]
    fn test_readlen_two_bytes() {
        let test_value = 300u16;

        let bytes = (test_value | 0x8000).to_be_bytes();

        eprintln!("{:x} {:x?}", test_value, bytes);

        let mut cursor = Cursor::new(&bytes[..]);

        let result = read_len(&mut cursor);

        assert!(result.is_ok());

        let value = result.unwrap();
        eprintln!("{:x}", value);

        assert_eq!(test_value as u32, value);
    }

    #[test]
    fn test_readlen_three_bytes() {
        let test_value = 0x4242u32;

        let bytes = (test_value | 0xC00000).to_be_bytes();
        eprintln!("{:x} {:x?}", test_value, &bytes[1..]);

        let mut cursor = Cursor::new(&bytes[1..]);

        let result = read_len(&mut cursor);

        assert!(result.is_ok());

        let value = result.unwrap();
        eprintln!("{:x}", value);

        assert_eq!(test_value, value);
    }

    #[test]
    fn test_readlen_four_bytes() {
        let test_value = 0x131337u32;

        let bytes = (test_value | 0xE0000000).to_be_bytes();
        eprintln!("{:x} {:x?}", test_value, &bytes[..]);

        let mut cursor = Cursor::new(&bytes[..]);

        let result = read_len(&mut cursor);

        assert!(result.is_ok());

        let value = result.unwrap();
        eprintln!("{:x}", value);

        assert_eq!(test_value, value);
    }

    #[test]
    fn test_readlen_five_bytes() {
        let test_value = 0xAABBCCDDu32;

        let mut bytes = test_value.to_be_bytes().to_vec();

        bytes.insert(0, 0xF0);

        eprintln!("{:x} {:x?}", test_value, &bytes[..]);

        let mut cursor = Cursor::new(&bytes[..]);

        let result = read_len(&mut cursor);

        assert!(result.is_ok());

        let value = result.unwrap();
        eprintln!("{:x}", value);

        assert_eq!(test_value, value);
    }

    #[test]
    fn test_readlen_invalid_prefix_bytes() {
        use crate::api::error::Error;

        for invalid_prefix in [0xF1, 0xF2, 0xF8, 0xFE, 0xFF] {
            let bytes = [invalid_prefix, 0x00, 0x00, 0x00, 0x00];
            let mut cursor = Cursor::new(&bytes[..]);

            let result = read_len(&mut cursor);
            match result {
                Err(Error::InvalidWordLength(b)) => assert_eq!(b, invalid_prefix),
                other => panic!(
                    "expected Err(InvalidWordLength({:#x})), got {:?}",
                    invalid_prefix, other
                ),
            }
        }
    }

    #[test]
    fn test_read_word_invalid_utf8_lossy() {
        use super::read_word;

        // Construct a word with invalid UTF-8 bytes (e.g. Latin-1 string: '=comment=café' in ISO-8859-1 with byte 0xE9)
        // Format: [len, payload]
        let mut raw = vec![];
        let payload = b"=comment=caf\xE9"; // 0xE9 is invalid UTF-8 without continuation
        raw.push(payload.len() as u8);
        raw.extend_from_slice(payload);

        let mut cursor = Cursor::new(&raw[..]);
        let word = read_word(&mut cursor)
            .expect("read_word should succeed via lossy conversion without panic/UB");

        assert_eq!(word, "=comment=caf\u{FFFD}");
    }

    #[test]
    fn test_read_sentence_with_mixed_utf8_and_binary() {
        use super::read_sentence;

        // Construct a sentence:
        // Word 1: "!re" (len 3)
        // Word 2: "=data=\xFF\xFE" (len 8, with invalid UTF-8)
        // Word 3: "" (len 0, sentence terminator)
        let mut raw = vec![];
        raw.push(3);
        raw.extend_from_slice(b"!re");

        let bin_word = b"=data=\xFF\xFE";
        raw.push(bin_word.len() as u8);
        raw.extend_from_slice(bin_word);

        raw.push(0); // Terminator

        let mut cursor = Cursor::new(&raw[..]);
        let sentence =
            read_sentence(&mut cursor).expect("failed to decode sentence with binary words");

        assert_eq!(sentence.len(), 3);
        assert_eq!(sentence[0], "!re");
        assert_eq!(sentence[1], "=data=\u{FFFD}\u{FFFD}");
        assert_eq!(sentence[2], "");
    }
}
