//! SHA-256（FIPS 180-4）纯软件实现，不依赖第三方 crate。
//!
//! 为什么自己写：SPEC 3.5 要求 `bin_hash` 是构建产物的**内容**哈希——内容变一个
//! 字节，哈希就必须变。CRC32 不行（碰撞太容易，且本项目用它做的是落盘完整性
//! 校验，不是指纹）。沙箱无网络、不许新增依赖，所以按 SPEC「需要算法自行实现」
//! 的既定做法自实现 SHA-256，并用 FIPS 标准测试向量锁死正确性。

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

const H0: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

/// 流式 SHA-256：`new()` → 若干次 `update()` → `finish()`。
/// 流式是为了不把整个固件读进内存（Phase 6 的 bin 可能上百 MiB）。
#[derive(Clone)]
pub struct Sha256 {
    state: [u32; 8],
    block: [u8; 64],
    block_len: usize,
    total_len: u64,
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha256 {
    pub fn new() -> Self {
        Self {
            state: H0,
            block: [0u8; 64],
            block_len: 0,
            total_len: 0,
        }
    }

    /// 追加一段数据。可任意切分，结果与一次性算完全一致。
    pub fn update(&mut self, mut data: &[u8]) {
        self.total_len += data.len() as u64;

        if self.block_len > 0 {
            let take = (64 - self.block_len).min(data.len());
            self.block[self.block_len..self.block_len + take].copy_from_slice(&data[..take]);
            self.block_len += take;
            data = &data[take..];
            if self.block_len == 64 {
                let block = self.block;
                compress(&mut self.state, &block);
                self.block_len = 0;
            }
        }

        let (chunks, rest) = data.as_chunks::<64>();
        for block in chunks {
            compress(&mut self.state, block);
        }
        if !rest.is_empty() {
            self.block[..rest.len()].copy_from_slice(rest);
            self.block_len = rest.len();
        }
    }

    /// 收尾（补位 + 长度）并输出 32 字节摘要。
    pub fn finish(mut self) -> [u8; 32] {
        // 先取出总长度：补位字节不计入消息长度。
        let bit_len = self.total_len.wrapping_mul(8);

        self.block[self.block_len] = 0x80; // block_len 恒 ≤ 63
        self.block_len += 1;
        if self.block_len > 56 {
            self.block[self.block_len..64].fill(0);
            let block = self.block;
            compress(&mut self.state, &block);
            self.block = [0u8; 64];
            self.block_len = 0;
        }
        self.block[self.block_len..56].fill(0);
        self.block[56..64].copy_from_slice(&bit_len.to_be_bytes());
        let block = self.block;
        compress(&mut self.state, &block);

        let mut out = [0u8; 32];
        for (i, word) in self.state.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        out
    }
}

fn compress(state: &mut [u32; 8], block: &[u8; 64]) {
    let mut w = [0u32; 64];
    for (i, chunk) in block.as_chunks::<4>().0.iter().enumerate() {
        w[i] = u32::from_be_bytes(*chunk);
    }
    for i in 16..64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16]
            .wrapping_add(s0)
            .wrapping_add(w[i - 7])
            .wrapping_add(s1);
    }

    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    for i in 0..64 {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & f) ^ ((!e) & g);
        let t1 = h
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(K[i])
            .wrapping_add(w[i]);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let t2 = s0.wrapping_add(maj);

        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }

    state[0] = state[0].wrapping_add(a);
    state[1] = state[1].wrapping_add(b);
    state[2] = state[2].wrapping_add(c);
    state[3] = state[3].wrapping_add(d);
    state[4] = state[4].wrapping_add(e);
    state[5] = state[5].wrapping_add(f);
    state[6] = state[6].wrapping_add(g);
    state[7] = state[7].wrapping_add(h);
}

/// 一次性算一段字节的 SHA-256。
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(data);
    h.finish()
}

/// 流式算一个文件内容的 SHA-256（不整块载入内存）。
pub fn sha256_file(path: &std::path::Path) -> std::io::Result<[u8; 32]> {
    use std::io::Read;

    let mut file = std::fs::File::open(path)?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut h = Sha256::new();
    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => h.update(&buf[..n]),
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(h.finish())
}

/// 小写十六进制。
pub fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0F) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn sha256_matches_fips_vectors() {
        // FIPS 180-4 / NIST 标准测试向量
        assert_eq!(
            to_hex(&sha256(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            to_hex(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            to_hex(&sha256(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn streaming_update_equals_one_shot() {
        let data: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
        let one_shot = sha256(&data);
        for chunk in [1usize, 3, 55, 63, 64, 65, 127, 333] {
            let mut h = Sha256::new();
            for part in data.chunks(chunk) {
                h.update(part);
            }
            assert_eq!(h.finish(), one_shot, "分块 {chunk} 必须与一次性结果一致");
        }
    }

    #[test]
    fn sha256_file_matches_memory_hash() {
        let dir = std::env::temp_dir().join(format!(
            "embhub-sha256-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("blob.bin");
        let data: Vec<u8> = (0..300_000u32).map(|i| (i * 7 % 253) as u8).collect();
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(&data).unwrap();
        drop(f);

        assert_eq!(sha256_file(&path).unwrap(), sha256(&data));
        assert_eq!(
            sha256_file(&dir.join("no-such.bin")).unwrap_err().kind(),
            std::io::ErrorKind::NotFound
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
