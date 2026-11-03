#!/usr/bin/env python3
"""Guard the permitted substitutions: changed outcomes, aliasing, and missing artifacts must still fail."""
import json
import tempfile
import unittest
from pathlib import Path

from compare_replay import compare_reference, exact, cli_times, adb_snapshot, compare_console, compare_adb


class ComparisonTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def tree(self, name, batch, allure, identity, time):
        root = self.root/name
        output = root/'output'
        (output/'allure-results').mkdir(parents=True)
        (output/'test_result').mkdir()
        (output/'bill').mkdir()
        (root/'transcript.json').write_text(json.dumps({'hooks':True,'hooksReport':{},'log':[{'batch':batch,'tests':['T#test']}],
                                                     'outcome':{'value':False},'errors':[]}))
        (output/'test_result/raw.json').write_text(json.dumps([{'status':'FAILURE','timestamp':0,'duration':1}]))
        (output/'test_result/test.json').write_text(json.dumps({'testBatchId':batch,'status':'FAILURE'}))
        (output/'allure-results'/f'{allure}-result.json').write_text(json.dumps({'uuid':allure,'fullName':'T.test','status':'failed'}))
        (output/'allure-results/environment.xml').write_text(f'<environment><parameter><key>sorting</key><value>com.malinskiy.marathon.config.strategy.SortingStrategyConfiguration$NoSortingStrategyConfiguration@{identity}</value></parameter><parameter><key>sorting</key><value>com.malinskiy.marathon.config.strategy.SortingStrategyConfiguration$NoSortingStrategyConfiguration@{identity}</value></parameter></environment>')
        (output/'bill/test.json').write_text(json.dumps({'start':time,'end':time+3,'duration':3,'device':'same'}))
        return root

    def pair(self):
        return (self.tree('a','00000000-0000-0000-0000-000000000001','10000000-0000-0000-0000-000000000001','abcd',1),
                self.tree('b','a0000000-0000-0000-0000-000000000001','b0000000-0000-0000-0000-000000000001','1234',99))

    def test_reference_substitutions_preserve_outcomes_and_aliases(self):
        a,b=self.pair()
        compare_reference(a,b,self.root/'diff')
        path = b/'output/allure-results/environment.xml'
        text = path.read_text(); path.write_text(text.replace('@1234', '@5678', 1))
        with self.assertRaises(AssertionError): compare_reference(a,b,self.root/'diff')

    def test_reference_rejects_changed_result_and_missing_file(self):
        a,b=self.pair()
        (b/'output/test_result/test.json').write_text('{"testBatchId":"a0000000-0000-0000-0000-000000000001","status":"PASSED"}')
        with self.assertRaises(AssertionError): compare_reference(a,b,self.root/'diff')
        (b/'output/test_result/test.json').unlink()
        with self.assertRaises(AssertionError): compare_reference(a,b,self.root/'diff')

    def test_billing_fields_must_exist_and_be_numeric(self):
        for bad in ({'device':'same'}, {'start':'bad','end':1,'duration':2}):
            a,b=self.pair()
            (b/'output/bill/test.json').write_text(json.dumps(bad))
            with self.assertRaises(AssertionError): compare_reference(a,b,self.root/'diff')
            import shutil
            shutil.rmtree(a); shutil.rmtree(b)

    def test_user_duration_metadata_is_preserved(self):
        a,b=self.pair()
        for root in (a,b):
            output = root/'output'
            (output/'devices').mkdir()
            (output/'devices/d.json').write_text('{"serialNumber":"host:1234:device"}')
            raw = [{'status':'FAILURE','timestamp':1,'duration':2,'deviceSerial':'host:1234:device',
                    'metaProperties':[{'name':'annotation','values':{'duration':'a'}}]}]
            (output/'test_result/raw.json').write_text(json.dumps(raw))
            test = output/'test_result/test.json'; value=json.loads(test.read_text())
            value.update(startTime=1,endTime=2); test.write_text(json.dumps(value))
            for path in (output/'allure-results').glob('*-result.json'):
                value=json.loads(path.read_text()); value.update(start=1,stop=2); path.write_text(json.dumps(value))
        compare_reference(a,b,self.root/'diff',cli=True)
        path=b/'output/test_result/raw.json'; value=json.loads(path.read_text())
        value[0]['metaProperties'][0]['values']['duration']='b'; path.write_text(json.dumps(value))
        with self.assertRaises(AssertionError): compare_reference(a,b,self.root/'diff',cli=True)
        with self.assertRaises(AssertionError): cli_times('test_result/raw.json', [{'timestamp':1}])
        self.assertEqual(cli_times('custom.json', {'duration':3}), {'duration':3})

    def test_config_shaped_annotation_literals_are_preserved(self):
        a,b=self.pair()
        for root, suffix in ((a, 'deadbeef'), (b, 'cafebabe')):
            path=root/'output/test_result/raw.json'
            rows=json.loads(path.read_text())
            rows[0]['metaProperties']=[{'name':'annotation','values':{
                'value':'com.malinskiy.marathon.config.Example@'+suffix,
                'array':'[Lcom.linkedin.dex.parser.DecodedValue;@'+suffix}}]
            path.write_text(json.dumps(rows))
        with self.assertRaises(AssertionError): compare_reference(a,b,self.root/'diff')

    def test_environment_user_identity_literals_are_preserved(self):
        a,b=self.pair()
        for root, suffix in ((a, 'deadbeef'), (b, 'cafebabe')):
            path=root/'output/allure-results/environment.xml'
            path.write_text(path.read_text().replace('</environment>',
                '<parameter><key>name</key><value>com.malinskiy.marathon.config.Example@'+suffix+'</value></parameter></environment>'))
        with self.assertRaises(AssertionError): compare_reference(a,b,self.root/'diff')

    def test_console_user_identity_literals_are_preserved(self):
        a,b=self.pair()
        for root, suffix in ((a, 'deadbeef'), (b, 'cafebabe')):
            (root/'stdout').write_text('annotation contact.foo@'+suffix+'\n')
            (root/'stderr').write_bytes(b'')
        with self.assertRaises(AssertionError): compare_console(a,b,self.root/'diff')

    def test_adb_request_order_after_preopened_transports(self):
        rows = [
            {'conn':1,'serial':None,'type':'request','service':'host:transport:d'},
            {'conn':2,'serial':None,'type':'request','service':'host:transport:d'},
            {'conn':1,'serial':'d','type':'request','service':'shell:install'},
            {'conn':2,'serial':'d','type':'request','service':'shell:instrument'},
            {'conn':1,'serial':'d','type':'shell','command':'install'},
            {'conn':2,'serial':'d','type':'shell','command':'instrument'}]
        def snapshot(rows):
            path = self.root/'adb.jsonl'; path.write_text(''.join(json.dumps(r)+'\n' for r in rows))
            return adb_snapshot(path)
        original = snapshot(rows)
        self.assertNotEqual(original, snapshot(rows[:2]+[rows[3],rows[2]]+rows[4:]))
        self.assertEqual(original, snapshot(rows[:4]+[rows[5],rows[4]]))
        independent = [r | {'serial':'other'} if r.get('serial') == 'd' and r['conn'] == 2 else
                       r | {'service':'host:transport:other'} if r['conn'] == 2 else r for r in rows]
        self.assertEqual(snapshot(independent), snapshot([independent[i] for i in (1,3,5,0,2,4)]))

    def test_adb_keeps_commands_payload_and_required_order(self):
        rows = [
            {'conn':1,'serial':None,'type':'request','service':'host:transport:d'},
            {'conn':1,'serial':'d','type':'request','service':'shell:first'},
            {'conn':1,'serial':'d','type':'shell','command':'first','stdin_bytes':2,'stdin_sha256':'a'*64},
            {'conn':2,'serial':None,'type':'request','service':'host:transport:d'},
            {'conn':2,'serial':'d','type':'request','service':'shell:second'}]
        def snapshot(rows):
            path = self.root/'adb.jsonl'; path.write_text(''.join(json.dumps(r)+'\n' for r in rows))
            return adb_snapshot(path)
        original = snapshot(rows)
        self.assertEqual(original, snapshot([r | {'conn':r['conn']+10} for r in rows]))
        self.assertNotEqual(original, snapshot(rows[3:]+rows[:3]))
        changed = [dict(r) for r in rows]; changed[2]['stdin_sha256']='b'*64
        self.assertNotEqual(original, snapshot(changed))
        changed = [dict(r) for r in rows]; changed[1]['service']='shell:first --different'
        self.assertNotEqual(original, snapshot(changed))
        self.assertNotEqual(original, snapshot(rows[:-1]))

    def test_allocated_uuid_in_user_annotation_is_literal(self):
        a,b=self.pair()
        for root in (a,b):
            path=root/'output/test_result/raw.json'; rows=json.loads(path.read_text())
            rows[0]['metaProperties']=[{'name':'annotation','values':{'value':'batch-0001'}}]
            path.write_text(json.dumps(rows))
        compare_reference(a,b,self.root/'diff')
        path=a/'output/test_result/raw.json'; rows=json.loads(path.read_text())
        rows[0]['metaProperties'][0]['values']['value']='00000000-0000-0000-0000-000000000001'
        path.write_text(json.dumps(rows))
        with self.assertRaises(AssertionError): compare_reference(a,b,self.root/'diff')

    def test_console_patterns_in_user_messages_are_literal(self):
        a,b=self.pair()
        for left,right in [('annotation 12:34:56.789', 'annotation 01:02:03.456'),
                           ('annotation 12 ms', 'annotation 13 ms'),
                           ('annotation batch-0001', 'annotation 00000000-0000-0000-0000-000000000001'),
                           ('at user value A', 'at user value B'),
                           ('java.class.path: /a/user.jar', 'java.class.path: /b/user.jar')]:
            for root,text in ((a,left),(b,right)):
                (root/'stdout').write_text(text+'\n'); (root/'stderr').write_bytes(b'')
            with self.subTest(left=left), self.assertRaises(AssertionError): compare_console(a,b,self.root/'diff')

    def test_device_serial_in_user_metadata_is_literal(self):
        a,b=self.pair()
        for root,serial in ((a,'host:1234:a'),(b,'host:1234:b')):
            output=root/'output'; (output/'devices').mkdir()
            (output/'devices/d.json').write_text(json.dumps({'serialNumber':serial}))
            (output/'test_result/raw.json').write_text(json.dumps([{'deviceSerial':serial,'timestamp':1,'duration':2,
                'metaProperties':[{'name':'annotation','values':{'value':serial}}]}]))
            path=output/'test_result/test.json'; value=json.loads(path.read_text());value.update(startTime=1,endTime=2);path.write_text(json.dumps(value))
            for path in (output/'allure-results').glob('*-result.json'):
                value=json.loads(path.read_text());value.update(start=1,stop=2);path.write_text(json.dumps(value))
        with self.assertRaises(AssertionError): compare_reference(a,b,self.root/'diff',cli=True)

    def test_console_user_line_order_and_opaque_json_are_preserved(self):
        a,b=self.pair()
        for root,text in ((a,'first\nsecond\n'),(b,'second\nfirst\n')):
            (root/'stdout').write_text(text);(root/'stderr').write_bytes(b'')
        with self.assertRaises(AssertionError): compare_console(a,b,self.root/'diff')
        for root,text in ((a,'{"value":1}'),(b,'{ "value": 1 }')):
            (root/'output/attachment.json').write_text(text)
        with self.assertRaises(AssertionError): compare_reference(a,b,self.root/'diff')

    def test_footer_words_and_extended_bridge_events_are_literal(self):
        a,b=self.pair()
        for root,word in ((a,'first'),(b,'second')):
            output=root/'output/html';output.mkdir()
            (output/'index.html').write_text('<p>user by Juno at '+word+'</div>\n')
        with self.assertRaises(AssertionError): compare_reference(a,b,self.root/'diff')
        for root in (a,b): (root/'output/html/index.html').unlink()
        path=a/'transcript.json';value=json.loads(path.read_text())
        value['log'].append({'t':0,'runBlocking':'core.Marathon.run','user':'visible'})
        path.write_text(json.dumps(value))
        with self.assertRaises(AssertionError): compare_reference(a,b,self.root/'diff')

    def test_live_sync_conflicting_writes_keep_cross_connection_order(self):
        import socket
        import struct
        import subprocess
        import time
        binary=Path(__file__).resolve().parents[2]/'target/debug/fake-adb'
        self.assertTrue(binary.is_file(), 'build fake-adb before running comparator tests')
        script=self.root/'sync.yaml'; script.write_text('server:\n  strict: true\ndevices:\n  - serial: d\n')
        def receive(sock,n):
            data=b''
            while len(data)<n:
                part=sock.recv(n-len(data))
                if not part: raise EOFError(data)
                data+=part
            return data
        def service(sock,text):
            data=text.encode();sock.sendall(f'{len(data):04x}'.encode()+data)
            self.assertEqual(receive(sock,4),b'OKAY')
        def request(sock,op,data): sock.sendall(op+struct.pack('<I',len(data))+data)
        def run(name,order):
            root=self.root/name;root.mkdir();port=root/'port'
            with (root/'stdout').open('wb') as out,(root/'stderr').open('wb') as err:
                process=subprocess.Popen([str(binary),'--script',str(script),'--port','0','--port-file',str(port),
                                          '--transcript',str(root/'adb.jsonl'),'--strict'],stdout=out,stderr=err)
                sockets=[]
                try:
                    deadline=time.monotonic()+60
                    while not port.exists():
                        self.assertIsNone(process.poll())
                        if time.monotonic()>deadline: raise TimeoutError('fake-adb startup')
                        time.sleep(.01)
                    for _ in range(2):
                        sock=socket.create_connection(('127.0.0.1',int(port.read_text())),timeout=3)
                        sockets.append(sock);service(sock,'host:transport:d');service(sock,'sync:')
                    for i in order:
                        sock=sockets[i];request(sock,b'SEND',b'/data/local/tmp/file,420')
                        request(sock,b'DATA',b'A' if i==0 else b'B');sock.sendall(b'DONE'+struct.pack('<I',0))
                        self.assertEqual(receive(sock,8),b'OKAY'+b'\0'*4)
                    request(sockets[0],b'RECV',b'/data/local/tmp/file')
                    self.assertEqual(receive(sockets[0],4),b'DATA')
                    value=receive(sockets[0],struct.unpack('<I',receive(sockets[0],4))[0])
                    self.assertEqual(receive(sockets[0],8),b'DONE'+b'\0'*4)
                finally:
                    for sock in sockets: sock.close()
                    process.terminate();self.assertEqual(process.wait(timeout=10),0)
            return root,value
        a,av=run('sync-a',[0,1]);b,bv=run('sync-b',[1,0])
        self.assertEqual((av,bv),(b'B',b'A'))
        with self.assertRaises(AssertionError): compare_adb(a,b)

    def test_repeated_runs_never_normalize_clocks_or_ids(self):
        a,b=self.pair()
        exact(a,a)
        with self.assertRaises(AssertionError): exact(a,b)


if __name__ == '__main__': unittest.main()
