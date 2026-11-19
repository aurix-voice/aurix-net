// Bit-level serializer for the gameplay framework: primitives, Unity math types, strings, byte
// arrays, enums, arrays/lists of those, PlayerID, NetworkIdentity references and INetSerializable.
using System;
using System.Collections;
using System.Collections.Generic;
using Aurix.Net.Managed;
using UnityEngine;

namespace Aurix.Net.Unity.Framework
{
    /// <summary>A player in the room: the Aurix actor id.</summary>
    public readonly struct PlayerID : IEquatable<PlayerID>
    {
        public readonly ushort id;

        public PlayerID(ushort id)
        {
            this.id = id;
        }

        public bool Equals(PlayerID owner) => id == owner.id;

        public override bool Equals(object target) => target is PlayerID player && player.id == id;

        public override int GetHashCode() => id;

        public override string ToString() => "P" + id;

        public static bool operator ==(PlayerID left, PlayerID right) => left.id == right.id;

        public static bool operator !=(PlayerID left, PlayerID right) => left.id != right.id;

        public static implicit operator ushort(PlayerID player) => player.id;
    }

    /// <summary>Implement on your own structs/classes to send them through SyncVars and RPCs.</summary>
    public interface INetSerializable
    {
        void Write(BitWriter writer);
        void Read(BitReader reader);
    }

    public static class NetSerializer
    {
        public const int MaxString = 65535,
            MaxBytes = 1 << 20,
            MaxCount = 1 << 16;

        /// <summary>Resolver for NetworkIdentity references; set by the NetworkManager.</summary>
        public static Func<uint, NetworkIdentity> ResolveIdentity;

        public static void Write(BitWriter writer, object value, Type type)
        {
            if (type.IsEnum)
            {
                type = Enum.GetUnderlyingType(type);
            }

            switch (Type.GetTypeCode(type))
            {
                case TypeCode.Boolean:
                    writer.WriteBool((bool)value);
                    return;
                case TypeCode.Byte:
                    writer.WriteBits((byte)value, 8);
                    return;
                case TypeCode.SByte:
                    writer.WriteBits((byte)(sbyte)value, 8);
                    return;
                case TypeCode.Int16:
                    writer.WriteBits((ushort)(short)value, 16);
                    return;
                case TypeCode.UInt16:
                    writer.WriteBits((ushort)value, 16);
                    return;
                case TypeCode.Int32:
                    writer.WriteBits((uint)(int)value, 32);
                    return;
                case TypeCode.UInt32:
                    writer.WriteBits((uint)value, 32);
                    return;
                case TypeCode.Int64:
                    writer.WriteBits((ulong)(long)value, 64);
                    return;
                case TypeCode.UInt64:
                    writer.WriteBits((ulong)value, 64);
                    return;
                case TypeCode.Single:
                    writer.WriteF32((float)value);
                    return;
                case TypeCode.Double:
                    writer.WriteBits((ulong)BitConverter.DoubleToInt64Bits((double)value), 64);
                    return;
                case TypeCode.Char:
                    writer.WriteBits((char)value, 16);
                    return;
                case TypeCode.String:
                    WriteString(writer, (string)value);
                    return;
            }
            if (type == typeof(Vector2))
            {
                var vector = (Vector2)value;
                writer.WriteF32(vector.x);
                writer.WriteF32(vector.y);
                return;
            }
            if (type == typeof(Vector3))
            {
                var vector = (Vector3)value;
                writer.WriteF32(vector.x);
                writer.WriteF32(vector.y);
                writer.WriteF32(vector.z);
                return;
            }
            if (type == typeof(Quaternion))
            {
                var rotation = (Quaternion)value;
                writer.WriteF32(rotation.x);
                writer.WriteF32(rotation.y);
                writer.WriteF32(rotation.z);
                writer.WriteF32(rotation.w);
                return;
            }
            if (type == typeof(Color))
            {
                var color = (Color)value;
                writer.WriteF32(color.r);
                writer.WriteF32(color.g);
                writer.WriteF32(color.b);
                writer.WriteF32(color.a);
                return;
            }
            if (type == typeof(PlayerID))
            {
                writer.WriteBits(((PlayerID)value).id, 16);
                return;
            }
            if (type == typeof(byte[]))
            {
                var bytes = (byte[])value ?? Array.Empty<byte>();
                writer.WriteUInt((ulong)bytes.Length, 20);
                writer.WriteBytes(bytes);
                return;
            }
            if (typeof(NetworkIdentity).IsAssignableFrom(type))
            {
                var id = (NetworkIdentity)value;
                writer.WriteBits(id != null && id.isSpawned ? id.objectId : 0u, 32);
                return;
            }
            if (typeof(INetSerializable).IsAssignableFrom(type))
            {
                bool has = value != null || type.IsValueType;
                writer.WriteBool(has);
                if (has)
                {
                    ((INetSerializable)value).Write(writer);
                }

                return;
            }
            if (type.IsArray)
            {
                var array = (Array)value;
                var et = type.GetElementType();
                writer.WriteUInt((ulong)(array?.Length ?? 0), 16);
                if (array != null)
                {
                    foreach (var element in array)
                    {
                        Write(writer, element, et);
                    }
                }

                return;
            }
            if (type.IsGenericType && type.GetGenericTypeDefinition() == typeof(List<>))
            {
                var list = (IList)value;
                var et = type.GetGenericArguments()[0];
                writer.WriteUInt((ulong)(list?.Count ?? 0), 16);
                if (list != null)
                {
                    foreach (var element in list)
                    {
                        Write(writer, element, et);
                    }
                }

                return;
            }
            if (type.IsGenericType && type.GetGenericTypeDefinition() == typeof(Nullable<>))
            {
                bool has = value != null;
                writer.WriteBool(has);
                if (has)
                {
                    Write(writer, value, Nullable.GetUnderlyingType(type));
                }

                return;
            }
            throw new NetException(
                "cannot serialize " + type.FullName + " (implement INetSerializable)"
            );
        }

        public static object Read(BitReader reader, Type type)
        {
            var orig = type;
            if (type.IsEnum)
            {
                type = Enum.GetUnderlyingType(type);
            }

            object target = null;
            switch (Type.GetTypeCode(type))
            {
                case TypeCode.Boolean:
                    target = reader.ReadBool();
                    break;
                case TypeCode.Byte:
                    target = (byte)reader.ReadBits(8);
                    break;
                case TypeCode.SByte:
                    target = (sbyte)(byte)reader.ReadBits(8);
                    break;
                case TypeCode.Int16:
                    target = (short)(ushort)reader.ReadBits(16);
                    break;
                case TypeCode.UInt16:
                    target = (ushort)reader.ReadBits(16);
                    break;
                case TypeCode.Int32:
                    target = (int)(uint)reader.ReadBits(32);
                    break;
                case TypeCode.UInt32:
                    target = (uint)reader.ReadBits(32);
                    break;
                case TypeCode.Int64:
                    target = (long)reader.ReadBits(64);
                    break;
                case TypeCode.UInt64:
                    target = reader.ReadBits(64);
                    break;
                case TypeCode.Single:
                    target = reader.ReadF32();
                    break;
                case TypeCode.Double:
                    target = BitConverter.Int64BitsToDouble((long)reader.ReadBits(64));
                    break;
                case TypeCode.Char:
                    target = (char)reader.ReadBits(16);
                    break;
                case TypeCode.String:
                    target = ReadString(reader);
                    break;
            }
            if (target != null)
            {
                return orig.IsEnum ? Enum.ToObject(orig, target) : target;
            }

            if (type == typeof(Vector2))
            {
                return new Vector2(reader.ReadF32(), reader.ReadF32());
            }

            if (type == typeof(Vector3))
            {
                return new Vector3(reader.ReadF32(), reader.ReadF32(), reader.ReadF32());
            }

            if (type == typeof(Quaternion))
            {
                return new Quaternion(
                    reader.ReadF32(),
                    reader.ReadF32(),
                    reader.ReadF32(),
                    reader.ReadF32()
                );
            }

            if (type == typeof(Color))
            {
                return new Color(
                    reader.ReadF32(),
                    reader.ReadF32(),
                    reader.ReadF32(),
                    reader.ReadF32()
                );
            }

            if (type == typeof(PlayerID))
            {
                return new PlayerID((ushort)reader.ReadBits(16));
            }

            if (type == typeof(byte[]))
            {
                int count = (int)reader.ReadUInt(20);
                if (count > MaxBytes)
                {
                    throw new NetException("byte[] too long");
                }

                return reader.ReadBytes(count);
            }
            if (typeof(NetworkIdentity).IsAssignableFrom(type))
            {
                uint id = (uint)reader.ReadBits(32);
                return id == 0 ? null : ResolveIdentity?.Invoke(id);
            }
            if (typeof(INetSerializable).IsAssignableFrom(type))
            {
                if (!reader.ReadBool())
                {
                    return null;
                }

                var serializable = (INetSerializable)Activator.CreateInstance(type);
                serializable.Read(reader);
                return serializable;
            }
            if (type.IsArray)
            {
                int count = (int)reader.ReadUInt(16);
                var et = type.GetElementType();
                var array = Array.CreateInstance(et, count);
                for (int i = 0; i < count; i++)
                {
                    array.SetValue(Read(reader, et), i);
                }

                return array;
            }
            if (type.IsGenericType && type.GetGenericTypeDefinition() == typeof(List<>))
            {
                int count = (int)reader.ReadUInt(16);
                var et = type.GetGenericArguments()[0];
                var list = (IList)Activator.CreateInstance(type);
                for (int i = 0; i < count; i++)
                {
                    list.Add(Read(reader, et));
                }

                return list;
            }
            if (type.IsGenericType && type.GetGenericTypeDefinition() == typeof(Nullable<>))
            {
                return reader.ReadBool() ? Read(reader, Nullable.GetUnderlyingType(type)) : null;
            }

            throw new NetException(
                "cannot deserialize " + type.FullName + " (implement INetSerializable)"
            );
        }

        public static void Write<T>(BitWriter writer, T value) => Write(writer, value, typeof(T));

        public static T Read<T>(BitReader reader) => (T)Read(reader, typeof(T));

        public static void WriteString(BitWriter writer, string text)
        {
            var bytes = System.Text.Encoding.UTF8.GetBytes(text ?? "");
            if (bytes.Length > MaxString)
            {
                throw new NetException("string too long");
            }

            writer.WriteUInt((ulong)bytes.Length, 16);
            writer.WriteBytes(bytes);
        }

        public static string ReadString(BitReader reader)
        {
            int count = (int)reader.ReadUInt(16);
            return System.Text.Encoding.UTF8.GetString(reader.ReadBytes(count));
        }

        public static bool AreEqual<T>(T left, T right) =>
            EqualityComparer<T>.Default.Equals(left, right);
    }
}
