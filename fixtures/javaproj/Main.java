/**
 * PrincessIDE Java fixture — canonical output for automated verification.
 *
 * The banner string "PrincessIDE java fixture ok" is a contract constant (D3):
 * any test that asserts on this fixture's stdout MUST match this exact text.
 */
public class Main {
    public static void main(String[] args) {
        System.out.println("PrincessIDE java fixture ok");
    }
}
