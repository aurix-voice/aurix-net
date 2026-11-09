// Minimal UnityEngine stand-ins so the Unity layer compiles under plain dotnet (no behaviour).
using System;
using System.Collections.Generic;
namespace UnityEngine
{
    public class Object { public string name; public static T Instantiate<T>(T o) where T : Object => o; public static void Destroy(Object o) { } }
    public class GameObject : Object { public T GetComponent<T>() where T : class => null; }
    public class Component : Object { Transform _t; public Transform transform => _t ??= new Transform(); public GameObject gameObject = new GameObject(); public void GetComponentsInChildren<T>(bool inc, List<T> r) { } public T GetComponent<T>() where T : class => null; }
    public class Rigidbody : Component { public Vector3 position, velocity, angularVelocity; public Quaternion rotation; public bool isKinematic; public void MovePosition(Vector3 p) { position = p; } public void MoveRotation(Quaternion q) { rotation = q; } }
    public enum AnimatorControllerParameterType { Float = 1, Int = 3, Bool = 4, Trigger = 9 }
    public class AnimatorControllerParameter { public string name; public AnimatorControllerParameterType type; }
    public class Animator : Component { public AnimatorControllerParameter[] parameters = new AnimatorControllerParameter[0]; public float GetFloat(string n) => 0; public void SetFloat(string n, float v) { } public void SetFloat(string n, float v, float damp, float dt) { } public int GetInteger(string n) => 0; public void SetInteger(string n, int v) { } public bool GetBool(string n) => false; public void SetBool(string n, bool v) { } public void SetTrigger(string n) { } }
    public class RequireComponentAttribute : Attribute { public RequireComponentAttribute(Type t) { } }
    public class Behaviour : Component { }
    public class MonoBehaviour : Behaviour { }
    public class ScriptableObject : Object { }
    public class Transform : Component { public Vector3 position, localPosition, localScale = new Vector3(1, 1, 1); public Quaternion rotation = Quaternion.identity, localRotation = Quaternion.identity; }
    public struct Vector2 { public float x, y; public Vector2(float a, float b) { x = a; y = b; } }
    public struct Vector3 { public float x, y, z; public Vector3(float a, float b, float c) { x = a; y = b; z = c; } public static Vector3 zero => default; public static Vector3 one => new Vector3(1, 1, 1); public float magnitude => (float)Math.Sqrt(x * x + y * y + z * z); public static Vector3 operator +(Vector3 a, Vector3 b) => new Vector3(a.x + b.x, a.y + b.y, a.z + b.z); public static Vector3 operator -(Vector3 a, Vector3 b) => new Vector3(a.x - b.x, a.y - b.y, a.z - b.z); public static Vector3 operator *(Vector3 a, float k) => new Vector3(a.x * k, a.y * k, a.z * k); public static bool operator ==(Vector3 a, Vector3 b) => a.x == b.x && a.y == b.y && a.z == b.z; public static bool operator !=(Vector3 a, Vector3 b) => !(a == b); public override bool Equals(object o) => o is Vector3 v && v == this; public override int GetHashCode() => x.GetHashCode() ^ y.GetHashCode() ^ z.GetHashCode(); public static float Distance(Vector3 a, Vector3 b) => (a - b).magnitude; public static Vector3 Lerp(Vector3 a, Vector3 b, float t) => a + (b - a) * Mathf.Clamp01(t); public override string ToString() => $"({x:0.##}, {y:0.##}, {z:0.##})"; }
    public struct Quaternion { public float x, y, z, w; public Quaternion(float a, float b, float c, float d) { x = a; y = b; z = c; w = d; } public static Quaternion identity => new Quaternion(0, 0, 0, 1); public static Quaternion Slerp(Quaternion a, Quaternion b, float t) { t = Mathf.Clamp01(t); float d = a.x * b.x + a.y * b.y + a.z * b.z + a.w * b.w; if (d < 0) { b = new Quaternion(-b.x, -b.y, -b.z, -b.w); } var q = new Quaternion(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t, a.z + (b.z - a.z) * t, a.w + (b.w - a.w) * t); float n = (float)Math.Sqrt(q.x * q.x + q.y * q.y + q.z * q.z + q.w * q.w); return n > 0 ? new Quaternion(q.x / n, q.y / n, q.z / n, q.w / n) : identity; } public static float Angle(Quaternion a, Quaternion b) { float d = Math.Abs(a.x * b.x + a.y * b.y + a.z * b.z + a.w * b.w); return d >= 1f ? 0f : (float)(Math.Acos(Math.Min(1, d)) * 2 * 57.29578); } public static Quaternion Euler(float x, float y, float z) { double cy = Math.Cos(z * 0.00872665), sy = Math.Sin(z * 0.00872665), cp = Math.Cos(y * 0.00872665), sp = Math.Sin(y * 0.00872665), cr = Math.Cos(x * 0.00872665), sr = Math.Sin(x * 0.00872665); return new Quaternion((float)(sr * cp * cy - cr * sp * sy), (float)(cr * sp * cy + sr * cp * sy), (float)(cr * cp * sy - sr * sp * cy), (float)(cr * cp * cy + sr * sp * sy)); } }
    public static class Mathf { public static float Min(float a, float b) => a < b ? a : b; public static float Clamp01(float v) => v < 0 ? 0 : v > 1 ? 1 : v; public static float Max(float a, float b) => a > b ? a : b; public static float Abs(float v) => Math.Abs(v); public static float Sqrt(float v) => (float)Math.Sqrt(v); public static float Lerp(float a, float b, float t) => a + (b - a) * Clamp01(t); }
    public static class SystemInfo { public static Rendering.GraphicsDeviceType graphicsDeviceType => Rendering.GraphicsDeviceType.Null; }
    namespace Rendering { public enum GraphicsDeviceType { Null = 4 } }
    public static class Time { public static float unscaledDeltaTime => 0.016f; public static float deltaTime => 0.016f; public static float unscaledTime => 0; }
    public static class Debug { public static void Log(object o) { Console.WriteLine(o); } public static void LogWarning(object o) { Console.WriteLine(o); } public static void LogError(object o) { Console.WriteLine(o); } }
    public class TooltipAttribute : Attribute { public TooltipAttribute(string s) { } }
    public class HeaderAttribute : Attribute { public HeaderAttribute(string s) { } }
    public class TextAreaAttribute : Attribute { public TextAreaAttribute(int a, int b) { } }
    public class SerializeFieldAttribute : Attribute { }
    public class HideInInspectorAttribute : Attribute { }
    public struct Color { public float r, g, b, a; public Color(float r, float g, float b, float a = 1) { this.r = r; this.g = g; this.b = b; this.a = a; } }
    public class CreateAssetMenuAttribute : Attribute { public string menuName, fileName; }
    public class DisallowMultipleComponentAttribute : Attribute { }
    public class DefaultExecutionOrderAttribute : Attribute { public DefaultExecutionOrderAttribute(int o) { } }
}
