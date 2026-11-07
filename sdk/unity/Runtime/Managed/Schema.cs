// IDL parser, same grammar as net-wire::schema (one statement per line, // comments).
using System;
using System.Collections.Generic;
using System.Globalization;

namespace Aurix.Net.Managed
{
    public enum FieldKind { Bool, UInt, Int, F32, Quant, Vec2, Vec3, Quat, String, Bytes }
    public sealed class FieldType { public FieldKind Kind; public int Bits; public float Min, Max, Res; public uint Max_; }
    public sealed class Field { public string Name; public FieldType Type; }
    public sealed class ComponentDef { public ushort Id; public string Name; public bool OwnerOnly, Global; public List<Field> Fields = new List<Field>(); public int IndexOf(string f) { for (int i = 0; i < Fields.Count; i++) if (Fields[i].Name == f) return i; return -1; } }

    public sealed class Schema
    {
        public List<ComponentDef> Components = new List<ComponentDef>();
        public ComponentDef ById(int id) => id >= 0 && id < Components.Count ? Components[id] : null;
        public ComponentDef ByName(string n) { foreach (var c in Components) if (c.Name == n) return c; return null; }
        public int ComponentIdBits => Math.Max(1, 64 - Bits.LeadingZeros((ulong)Math.Max(2, Components.Count) - 1));

        public static Schema Parse(string text)
        {
            var s = new Schema(); ComponentDef cur = null; int line = 0;
            foreach (var raw in text.Split('\n'))
            {
                line++;
                var l = raw; int c = l.IndexOf("//", StringComparison.Ordinal); if (c >= 0) l = l.Substring(0, c); l = l.Trim();
                if (l.Length == 0) continue;
                if (l.StartsWith("component "))
                {
                    if (cur != null) throw new NetException($"schema line {line}: nested component");
                    var rest = l.Substring(10).Trim();
                    if (!rest.EndsWith("{")) throw new NetException($"schema line {line}: expected '{{'");
                    var parts = rest.Substring(0, rest.Length - 1).Trim().Split(new[] { ' ', '\t' }, StringSplitOptions.RemoveEmptyEntries);
                    cur = new ComponentDef { Id = (ushort)s.Components.Count, Name = parts[0] };
                    for (int i = 1; i < parts.Length; i++) { if (parts[i] == "@owner_only") cur.OwnerOnly = true; else if (parts[i] == "@global") cur.Global = true; else throw new NetException($"schema line {line}: unknown flag {parts[i]}"); }
                    continue;
                }
                if (l == "}") { if (cur == null) throw new NetException($"schema line {line}: stray '}}'"); s.Components.Add(cur); cur = null; continue; }
                if (cur == null) throw new NetException($"schema line {line}: field outside component");
                int colon = l.IndexOf(':'); if (colon < 0) throw new NetException($"schema line {line}: expected name: type");
                var name = l.Substring(0, colon).Trim(); var ty = l.Substring(colon + 1).Trim();
                string attr = null; int at = ty.IndexOf('@'); if (at >= 0) { attr = ty.Substring(at + 1).Trim(); ty = ty.Substring(0, at).Trim(); }
                cur.Fields.Add(new Field { Name = name, Type = ParseType(ty, attr, line) });
            }
            if (cur != null) throw new NetException("schema: unterminated component");
            return s;
        }

        static FieldType ParseType(string b, string attr, int line)
        {
            FieldType Range(FieldKind k)
            {
                if (attr == null || !attr.StartsWith("range(") || !attr.EndsWith(")")) throw new NetException($"schema line {line}: {b} needs @range(min, max, res)");
                var p = attr.Substring(6, attr.Length - 7).Split(',');
                if (p.Length != 3) throw new NetException($"schema line {line}: bad range");
                return new FieldType { Kind = k, Min = F(p[0]), Max = F(p[1]), Res = F(p[2]) };
            }
            float F(string x) => float.Parse(x.Trim(), CultureInfo.InvariantCulture);
            uint? Paren(string n) { if (!b.StartsWith(n)) return null; var r = b.Substring(n.Length); if (r.Length == 0) return 0; if (r.StartsWith("(") && r.EndsWith(")") && uint.TryParse(r.Substring(1, r.Length - 2).Trim(), out var v)) return v; throw new NetException($"schema line {line}: bad {n} size"); }
            switch (b)
            {
                case "bool": return new FieldType { Kind = FieldKind.Bool };
                case "u8": return new FieldType { Kind = FieldKind.UInt, Bits = 8 };
                case "u16": return new FieldType { Kind = FieldKind.UInt, Bits = 16 };
                case "u32": return new FieldType { Kind = FieldKind.UInt, Bits = 32 };
                case "u64": return new FieldType { Kind = FieldKind.UInt, Bits = 64 };
                case "i8": return new FieldType { Kind = FieldKind.Int, Bits = 8 };
                case "i16": return new FieldType { Kind = FieldKind.Int, Bits = 16 };
                case "i32": return new FieldType { Kind = FieldKind.Int, Bits = 32 };
                case "i64": return new FieldType { Kind = FieldKind.Int, Bits = 64 };
                case "f32": return attr == null ? new FieldType { Kind = FieldKind.F32 } : Range(FieldKind.Quant);
                case "vec3": return Range(FieldKind.Vec3);
                case "vec2": return Range(FieldKind.Vec2);
                case "quat": return new FieldType { Kind = FieldKind.Quat };
            }
            var bits = Paren("bits"); if (bits.HasValue) { if (bits == 0 || bits > 64) throw new NetException($"schema line {line}: bits(n) needs 1..=64"); return new FieldType { Kind = FieldKind.UInt, Bits = (int)bits.Value }; }
            var str = Paren("string"); if (str.HasValue) return new FieldType { Kind = FieldKind.String, Max_ = Math.Min(Math.Max(str.Value, 1), 65535) };
            var by = Paren("bytes"); if (by.HasValue) return new FieldType { Kind = FieldKind.Bytes, Max_ = Math.Min(Math.Max(by.Value, 1), 65535) };
            throw new NetException($"schema line {line}: unknown type {b}");
        }
    }
}
