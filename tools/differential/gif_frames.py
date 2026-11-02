"""Small strict GIF89a decoder for composited differential frames (no tolerance)."""
import base64
import struct
import evidence as e


def _decode(data):
    e.require(data[:6] in (b'GIF89a',b'GIF87a'), 'invalid GIF header')
    width,height,flags,background,aspect = struct.unpack('<HHBBB',data[6:13])
    e.require(0 < width*height <= 16_777_216, 'GIF dimensions outside capture limit')
    cursor = 13
    def take(n):
        nonlocal cursor
        out = data[cursor:cursor+n]; cursor += n
        e.require(len(out) == n, 'truncated GIF')
        return out
    def palette(flags):
        return [tuple(take(3)) for _ in range(2**((flags&7)+1))] if flags&128 else None
    global_colors = palette(flags)
    def blocks():
        out = b''
        while True:
            size = take(1)[0]
            if not size: return out
            out += take(size)
    canvas = [(0,0,0,0)] * (width*height)
    frames, extensions = [], []
    gce = (0,0,None,False)
    loop = None
    while True:
        tag = take(1)[0]
        if tag == 0x3b: break
        if tag == 0x21:
            label = take(1)[0]
            if label == 0xf9:
                e.require(take(1) == b'\x04', 'invalid GIF control')
                control,delay,transparent = struct.unpack('<BHB',take(4))
                e.require(take(1) == b'\0', 'invalid GIF control terminator')
                gce = ((control>>2)&7,delay,transparent if control&1 else None,bool(control&2))
            else:
                payload = blocks()
                if label == 0xff and payload.startswith(b'NETSCAPE2.0\x01'):
                    e.require(len(payload) == 14,'invalid GIF loop extension')
                    loop = struct.unpack('<H',payload[-2:])[0]
                else:
                    extensions.append([label,base64.b64encode(payload).decode()])
            continue
        e.require(tag == 0x2c, 'unknown GIF block')
        left,top,w,h,local_flags = struct.unpack('<HHHHB',take(9))
        e.require(w and h and left+w <= width and top+h <= height,'invalid GIF frame rectangle')
        colors = palette(local_flags) or global_colors
        e.require(colors is not None,'missing GIF palette')
        minimum = take(1)[0]
        e.require(2 <= minimum <= 8,'invalid GIF LZW minimum')
        compressed = blocks(); bit = 0
        clear,stop = 1<<minimum,(1<<minimum)+1
        table = {}; size = minimum+1; previous = None; pixels = []
        while True:
            e.require(bit+size <= len(compressed)*8,'truncated GIF LZW')
            code = (int.from_bytes(compressed[bit//8:bit//8+3],'little') >> (bit%8)) & ((1<<size)-1)
            bit += size
            if code == clear:
                table = {i:bytes([i]) for i in range(clear)}
                next_code = stop+1; size = minimum+1; previous = None
                continue
            if code == stop: break
            value = table.get(code)
            if value is None:
                e.require(previous is not None and code == next_code,'invalid GIF LZW code')
                value = previous+previous[:1]
            pixels.extend(value)
            e.require(len(pixels) <= w*h,'excess GIF pixels')
            if previous is not None and next_code < 4096:
                table[next_code] = previous+value[:1]; next_code += 1
                if next_code == 1<<size and size < 12: size += 1
            previous = value
        e.require(len(pixels) == w*h,'missing GIF pixels')
        before = canvas[:]
        disposal,delay,transparent,user_input = gce
        rows = list(range(h)) if not local_flags&64 else [y for start,step in [(0,8),(4,8),(2,4),(1,2)] for y in range(start,h,step)]
        for row,y in enumerate(rows):
            for x,index in enumerate(pixels[row*w:(row+1)*w]):
                e.require(index < len(colors),'invalid GIF palette index')
                if index != transparent: canvas[(top+y)*width+left+x] = (*colors[index],255)
        frames.append(dict(rect=[left,top,w,h],delay=delay,disposal=disposal,
                           transparent=transparent is not None,user_input=user_input,rgba_sha256=e.sha(bytes(c for p in canvas for c in p))))
        if disposal == 2:
            color = (*global_colors[background],255) if global_colors and background != transparent else (0,0,0,0)
            for y in range(top,top+h): canvas[y*width+left:y*width+left+w] = [color]*w
        elif disposal == 3: canvas = before
        else: e.require(disposal in (0,1),'unknown GIF disposal')
        gce = (0,0,None,False)
    e.require(cursor == len(data) and frames,'trailing/empty GIF')
    return dict(version=data[:6].decode(),width=width,height=height,aspect=aspect,loop=loop,frames=frames,extensions=extensions)



def decode(data):
    try:
        return _decode(data)
    except (ValueError,IndexError,OverflowError,struct.error) as error:
        raise e.Invalid('invalid GIF: ' + str(error)) from error
