use std::io;

use super::datagram::{DatagramReceiver, ReadAdapter};
use super::ipv4_header::{IPv4Header, IPv4HeaderData, IPv4HeaderMut};
use super::ipv4_packet::{IPv4Packet, MAX_PACKET_LENGTH};
use super::transport_header::{TransportHeader, TransportHeaderData, TransportHeaderMut};

/// Convert from level 5 to level 3 by appending correct IP and transport headers.
pub struct Packetizer {
    buffer: Box<[u8; MAX_PACKET_LENGTH]>,
    transport_index: usize,
    payload_index: usize,
    ipv4_header_data: IPv4HeaderData,
    transport_header_data: TransportHeaderData,
}

impl Packetizer {
    pub fn new(reference_ipv4_header: &IPv4Header, reference_transport_header: &TransportHeader) -> Self {
        let mut buffer = Box::new([0; MAX_PACKET_LENGTH]);

        let transport_index = reference_ipv4_header.header_length() as usize;
        let payload_index = transport_index + reference_transport_header.header_length() as usize;

        let mut ipv4_header_data = reference_ipv4_header.data().clone();
        let mut transport_header_data = reference_transport_header.data_clone();

        {
            let ipv4_header_raw = &mut buffer[..transport_index];
            ipv4_header_raw.copy_from_slice(reference_ipv4_header.raw());
            let mut ipv4_header = ipv4_header_data.bind_mut(ipv4_header_raw);
            ipv4_header.swap_source_and_destination();
        }

        {
            let transport_header_raw = &mut buffer[transport_index..payload_index];
            transport_header_raw.copy_from_slice(reference_transport_header.raw());
            let mut transport_header = transport_header_data.bind_mut(transport_header_raw);
            transport_header.swap_source_and_destination();
        }

        Self {
            buffer: buffer,
            transport_index: transport_index,
            payload_index: payload_index,
            ipv4_header_data: ipv4_header_data,
            transport_header_data: transport_header_data,
        }
    }

    pub fn packetize_empty_payload(&mut self) -> IPv4Packet {
        self.build(0)
    }

    pub fn packetize<R: DatagramReceiver>(&mut self, source: &mut R) -> io::Result<IPv4Packet> {
        let r = source.recv(&mut self.buffer[self.payload_index..])?;
        let ipv4_packet = self.build(r as u16);
        Ok(ipv4_packet)
    }

    pub fn packetize_read<R: io::Read>(&mut self, source: &mut R, max_chunk_size: Option<usize>) -> io::Result<IPv4Packet> {
        // FIXME handle EOF (read returning 0) for TCP stream
        let mut adapter = ReadAdapter::new(source, max_chunk_size);
        self.packetize(&mut adapter)
    }

    pub fn ipv4_header_mut(&mut self) -> IPv4HeaderMut {
        let raw = &mut self.buffer[..self.transport_index];
        self.ipv4_header_data.bind_mut(raw)
    }

    pub fn transport_header_mut(&mut self) -> TransportHeaderMut {
        let raw = &mut self.buffer[self.transport_index..self.payload_index];
        self.transport_header_data.bind_mut(raw)
    }

    fn build(&mut self, payload_length: u16) -> IPv4Packet {
        let total_length = self.payload_index as u16 + payload_length;

        self.ipv4_header_mut().set_total_length(total_length);
        self.transport_header_mut().set_payload_length(payload_length);

        let mut ipv4_packet = IPv4Packet::new(&mut self.buffer[..total_length as usize], self.ipv4_header_data.clone(), self.transport_header_data.clone());
        ipv4_packet.compute_checksums();
        ipv4_packet
    }

    pub fn inflate(&mut self, packet_length: u16) -> IPv4Packet {
        IPv4Packet::new(&mut self.buffer[..packet_length as usize], self.ipv4_header_data.clone(), self.transport_header_data.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use byteorder::{BigEndian, WriteBytesExt};
    use relay::datagram::tests::MockDatagramSocket;

    fn create_packet() -> Vec<u8> {
        let mut raw = Vec::new();
        raw.write_u8(4u8 << 4 | 5).unwrap();
        raw.write_u8(0).unwrap(); // ToS
        raw.write_u16::<BigEndian>(32).unwrap(); // total length 20 + 8 + 4
        raw.write_u32::<BigEndian>(0).unwrap(); // id_flags_fragment_offset
        raw.write_u8(0).unwrap(); // TTL
        raw.write_u8(17).unwrap(); // protocol (UDP)
        raw.write_u16::<BigEndian>(0).unwrap(); // checksum
        raw.write_u32::<BigEndian>(0x12345678).unwrap(); // source address
        raw.write_u32::<BigEndian>(0x42424242).unwrap(); // destination address

        raw.write_u16::<BigEndian>(1234).unwrap(); // source port
        raw.write_u16::<BigEndian>(5678).unwrap(); // destination port
        raw.write_u16::<BigEndian>(12).unwrap(); // length
        raw.write_u16::<BigEndian>(0).unwrap(); // checksum

        raw.write_u32::<BigEndian>(0x11223344).unwrap(); // payload
        raw
    }

    #[test]
    fn merge_headers_and_payload() {
        let mut raw = &mut create_packet()[..];
        let reference_packet = IPv4Packet::parse(raw);

        let data = [ 0x11u8, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88 ];
        let mut mock = MockDatagramSocket::from_data(&data);

        let ipv4_header = reference_packet.ipv4_header();
        let transport_header = reference_packet.transport_header().unwrap();
        let mut packetizer = Packetizer::new(&ipv4_header, &transport_header);

        let packet = packetizer.packetize(&mut mock).unwrap();
        assert_eq!(36, packet.ipv4_header().total_length());
        assert_eq!(data, &packet.raw()[28..36]);
    }

    #[test]
    fn last_packet() {
        let mut raw = &mut create_packet()[..];
        let reference_packet = IPv4Packet::parse(raw);

        let data = [ 0x11u8, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88 ];
        let mut mock = MockDatagramSocket::from_data(&data);

        let ipv4_header = reference_packet.ipv4_header();
        let transport_header = reference_packet.transport_header().unwrap();
        let mut packetizer = Packetizer::new(&ipv4_header, &transport_header);

        let packet_length = packetizer.packetize(&mut mock).unwrap().length();
        let packet = packetizer.inflate(packet_length);
        assert_eq!(36, packet.ipv4_header().total_length());
        assert_eq!(data, &packet.raw()[28..36]);
    }

    #[test]
    fn packetize_chunks() {
        let mut raw = &mut create_packet()[..];
        let reference_packet = IPv4Packet::parse(raw);

        let data = [ 0x11u8, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88 ];
        let mut cursor = io::Cursor::new(&data);

        let ipv4_header = reference_packet.ipv4_header();
        let transport_header = reference_packet.transport_header().unwrap();
        let mut packetizer = Packetizer::new(&ipv4_header, &transport_header);

        {
            let packet = packetizer.packetize_read(&mut cursor, Some(2)).unwrap();
            assert_eq!(30, packet.ipv4_header().total_length());
            assert_eq!([0x11, 0x22], packet.payload().unwrap());
        }

        {
            let packet = packetizer.packetize_read(&mut cursor, Some(3)).unwrap();
            assert_eq!(31, packet.ipv4_header().total_length());
            assert_eq!([0x33, 0x44, 0x55], packet.payload().unwrap());
        }

        {
            let packet = packetizer.packetize_read(&mut cursor, Some(1024)).unwrap();
            assert_eq!(31, packet.ipv4_header().total_length());
            assert_eq!([0x66, 0x77, 0x88], packet.payload().unwrap());
        }
    }
}
