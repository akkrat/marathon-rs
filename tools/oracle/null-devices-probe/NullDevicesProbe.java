import com.fasterxml.jackson.core.type.TypeReference;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.dataformat.yaml.YAMLFactory;
import com.malinskiy.marathon.apple.configuration.Marathondevices;

/**
 * WP 2.4 review round 1: the helpful NPE of a null Marathondevices document in
 * AppleMacosProvider.initialize, whose bytecode is readValue(String, TypeReference),
 * checkcast Marathondevices, invokevirtual getWorkers (javap of macos-*.jar).
 * AppleSimulatorProvider stores the result in the local `marathonfile` first
 * (LocalVariableTable slot 5), hence its `because "marathonfile" is null`.
 */
public class NullDevicesProbe {
    public static void main(String[] args) throws Exception {
        ObjectMapper mapper = new ObjectMapper(new YAMLFactory());
        try {
            ((Marathondevices) mapper.readValue("null", new TypeReference<Marathondevices>() {})).getWorkers();
        } catch (NullPointerException e) {
            System.out.println("MACOS " + e.getClass().getName() + ": " + e.getMessage());
        }
    }
}
