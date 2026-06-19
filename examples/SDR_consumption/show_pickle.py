
import pickle
import sys

import matplotlib.pyplot as plt
with open(sys.argv[1], "rb") as f:
        pickle.load(f) 
plt.show()