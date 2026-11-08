#!/usr/bin/env python3
"""Offline generator regressions, including the measured Kotlin constructor metadata."""
import hashlib
import json
from pathlib import Path
import unittest
import sys
sys.path.insert(0, str(Path(__file__).resolve().parent))
import generate_xcresult as g


def prop(name, target, optional=False, internal=False, wrapped=None):
    return dict(name=name, type=target, isOptional=optional, isInternal=internal, wrappedType=wrapped)


def obj(name, fields=(), parent=None):
    return dict(kind='object', type=dict(name=name, **({'supertype': parent} if parent else {})), properties=list(fields))


class Generator(unittest.TestCase):
    def test_pinned_schema_matches_measured_kotlin_constructors(self):
        schema = json.loads(g.SCHEMA.read_bytes())
        self.assertEqual(schema['version'], {'major': 3, 'minor': 39})
        self.assertEqual(schema['signature'], 'PS0lrtUj8Aw=')
        self.assertEqual([sum(t['kind'] == k for t in schema['types']) for k in ('object', 'value', 'array')], [57, 5, 1])
        objects = g.resolve(schema)
        measured = json.loads((g.ROOT / 'fixtures/oracle/xcresult/models.json').read_text())
        self.assertEqual(set(objects), set(measured))
        types = {'String': 'kotlin.String', 'Int': 'kotlin.Int', 'Double': 'kotlin.Double', 'Bool': 'kotlin.Boolean', 'Date': 'java.util.Date'}
        for name, fields in objects.items():
            expected = []
            for field in fields:
                target = g.unwrapped(field)
                kind = types.get(target, 'com.malinskiy.marathon.vendor.ios.xcrun.xcresulttool.' + target)
                array = field['type'] == 'Array'
                if array:
                    kind = 'kotlin.collections.List<' + kind + '>'
                optional = field['isOptional']
                if optional:
                    kind += '?'
                default = not optional and (array or target in ('String', 'Int', 'Double', 'Bool'))
                expected.append(dict(name=field['name'], type=kind, default=default))
            self.assertEqual(expected, measured[name], name)

    def test_owned_output_is_reproducible(self):
        self.assertEqual(g.generate(), g.OUTPUT.read_text())
        self.assertEqual(g.generate(), g.generate())
        self.assertIn(hashlib.sha256(g.SCHEMA.read_bytes()).hexdigest(), g.OUTPUT.read_text())

    def test_self_reference_and_flattening_filtering(self):
        fields = [prop('base', 'String'), prop('ignored', 'SchemaSerializable'), prop('internal', 'Missing', internal=True)]
        schema = dict(types=[obj('Child', [prop('self', 'Optional', True, wrapped='Child')], 'Base'), obj('Base', fields)])
        objects = g.resolve(schema)
        self.assertEqual([p['name'] for p in objects['Child']], ['self', 'base'])
        self.assertEqual(g.rust_type('Child', objects['Child'][0]), 'Option<Box<Child>>')
        self.assertEqual(g.rust_type('Any', prop('list', 'Array', True, wrapped='String')), 'Option<Vec<Option<String>>>')

    def test_unknown_kind_and_unresolved_dependencies_fail(self):
        with self.assertRaisesRegex(ValueError, 'Unknown xcresulttool type kind'):
            g.resolve(dict(types=[dict(kind='future')]))
        with self.assertRaisesRegex(ValueError, "Can't resolve types"):
            g.resolve(dict(types=[obj('Missing', [prop('value', 'Undefined')])]))
        with self.assertRaisesRegex(ValueError, 'Unresolved property type Reference'):
            g.resolve(dict(types=[obj('Owner', [prop('ref', 'Reference')])]))
        with self.assertRaisesRegex(ValueError, "Can't resolve types"):
            g.resolve(dict(types=[obj('A', [prop('b', 'B')]), obj('B', [prop('a', 'A')])]))


if __name__ == '__main__':
    unittest.main()
