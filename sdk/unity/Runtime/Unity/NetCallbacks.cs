// Spawn/despawn callbacks on any component of a replicated prefab (Fusion's ISpawned/IDespawned).
namespace Aurix.Net.Unity
{
    /// <summary>Called right after the view is bound: entity id, owner and [Sync] fields are valid.</summary>
    public interface ISpawned { void Spawned(NetView view); }
    /// <summary>Called before the view's GameObject is destroyed because the entity left the world.</summary>
    public interface IDespawned { void Despawned(NetView view); }
}
