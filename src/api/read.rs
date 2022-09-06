use std::io::Cursor;

use bytes::Buf;

use super::error::Error;

fn get_byte(cursor: &mut Cursor<&[u8]>) -> Option<u8> {
    cursor.has_remaining().then(|| cursor.get_u8())
}

fn read_len(cursor: &mut Cursor<&[u8]>) -> Result<u32, Error> {
    let mut next_byte = || get_byte(cursor).ok_or(Error::Incomplete);

    let first_byte = next_byte()?;

    if first_byte >> 7 == 0b0 {
        return Ok(first_byte as u32);
    }

    let mut data: [u8; 4] = [0; 4];

    if first_byte >> 6 == 0b10 {
        data[0] = first_byte & !0xC0;
        data[1] = next_byte()?;

        return Ok(u32::from_ne_bytes(data));
    }

    if first_byte >> 5 == 0b110 {
        data[0] = first_byte & !0xE0;
        data[1] = next_byte()?;
        data[2] = next_byte()?;

        return Ok(u32::from_ne_bytes(data));
    }

    if first_byte >> 4 == 0b1110 {
        data[0] = first_byte & !0xF0;
        data[1] = next_byte()?;
        data[2] = next_byte()?;
        data[3] = next_byte()?;

        return Ok(u32::from_ne_bytes(data));
    }

    if first_byte == 0xF0 {
        data[0] = next_byte()?;
        data[1] = next_byte()?;
        data[2] = next_byte()?;
        data[3] = next_byte()?;

        return Ok(u32::from_ne_bytes(data));
    }

    unreachable!()
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

fn read_word<'buf>(cursor: &mut Cursor<&'buf [u8]>) -> Result<&'buf str, Error> {
    let str_len = read_len(cursor)?;

    let str_bytes = read_bytes(cursor, str_len)?;

    let text = unsafe { core::str::from_utf8_unchecked(str_bytes) };

    Ok(text)
}

pub fn read_sentence<'buf>(cursor: &mut Cursor<&'buf [u8]>) -> Result<Vec<&'buf str>, Error> {
    let mut sentence = vec![];

    loop {
        match read_word(cursor)? {
            empty @ "" => {
                sentence.push(empty);
                break;
            }

            word => sentence.push(word),
        }
    }

    Ok(sentence)
}
